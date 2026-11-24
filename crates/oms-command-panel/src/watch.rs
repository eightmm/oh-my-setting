use crate::{
    Error, Output, Result, Services, attention,
    board_refresh::{BoardRefresh, ChatReads, RefreshKey},
    cli::Options,
    commands, input, navigation,
    orchestrator::{ChatBinding, DetailUsageSelection, ObservationGeneration, Observations, Panel},
    render,
};
use oms_panel_core::{
    NativeCommand, Value, eventlog,
    services::{ExecutionRequest, PeerExecutor},
    state::Snapshot,
};
use serde_json::json;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

pub fn run(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<Output> {
    run_with_source(repo, Path::new(""), options, services, interrupted)
}

pub(crate) fn run_with_source(
    repo: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<Output> {
    let mut output = std::io::stdout().lock();
    run_with_source_writer(repo, source, options, services, interrupted, &mut output)
}
/// Watch output is streamed rather than retained in an unbounded command result.
pub fn run_with_writer(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    interrupted: &AtomicBool,
    output: &mut dyn Write,
) -> Result<Output> {
    run_with_source_writer(repo, Path::new(""), options, services, interrupted, output)
}

fn run_with_source_writer(
    repo: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    interrupted: &AtomicBool,
    output: &mut dyn Write,
) -> Result<Output> {
    let logging = options.has("log");
    let interactive = std::io::stdout().is_terminal() && !logging;
    let colored = render::color_enabled(interactive);
    let mut terminal = if interactive {
        input::TerminalInput::open()?
    } else {
        None
    };
    if let Some(terminal) = &mut terminal {
        terminal.enable_drag()?;
    }
    let mut nav = navigation::Navigation {
        view: crate::layout::View::parse(options.get("view").unwrap_or("auto"))?,
        attention_only: options.has("attention-only"),
        ..Default::default()
    };
    let max_count = options
        .get("count")
        .map(|_| options.integer("count", 1, 1, 1_000_000))
        .transpose()?;
    let follow = options.has("watch") || (logging && options.has("follow"));
    // Only a watcher that will wait between refreshes listens for wakeups.
    let wake = (follow || terminal.is_some())
        .then(|| crate::wakeup::Wakeup::bind(repo))
        .flatten();
    let configured_room = commands::room(options);
    let mut room = configured_room.clone().unwrap_or_default();
    let mut target = BoardTarget {
        room: configured_room,
        caller: std::env::var("TMUX_PANE").ok(),
        managed: false,
    };
    let mut board = (options.has("watch") && max_count.is_none() && terminal.is_some())
        .then(|| InteractiveRefresh::start(services.workspace_group_resolver()))
        .transpose()?;
    if board.is_some() {
        room.clear();
    }
    let mut requested = true;
    #[cfg(target_os = "linux")]
    let mut agy_first_child_wake = crate::wakeup::FirstChildWake::default();
    #[cfg(target_os = "linux")]
    let mut codex_roster_wake = crate::wakeup::FirstChildWake::default();
    let mut deadline = Instant::now();
    let mut scope_sample = None;
    let mut read = attention::load(repo, &room);
    let mut previous: Option<Value> = None;
    let mut log = eventlog::EventLog::default();
    let mut ticks = 0;
    let mut status = 0;
    let session = board.is_none().then(commands::Session::new);
    let mut last_frame: Vec<String> = vec![];
    let mut last_size = (0, 0);
    let mut provider_rows = [None; 3];
    let mut worker_rows = std::collections::BTreeMap::new();
    let mut weekly_hit = None;
    let mut main_switch_hit = None;
    let mut weekly = WeeklyUsage::default();
    let mut chat: Option<ChatDock> = None;
    let mut split = DockSplit::default();
    let mut observations = Observations::default();
    let mut main_footer = MainFooter::default();
    let mut main_checkout = MainCheckout::default();
    let mut selection_changed = SelectionFence::None;
    let mut switch = MainSwitch::default();
    let mut pending_detail_usage: Option<DetailUsageRequest> = None;
    let mut retained_detail_usage: Option<DetailUsageEvidence> = None;
    loop {
        if interrupted.load(Ordering::Acquire) {
            return Ok((130, vec![], vec![]));
        }
        let mut before = previous.as_ref().map(|old| render::visible(old, &nav));
        let mut selected_chat = before
            .as_deref()
            .and_then(|calls| calls.get(nav.selected))
            .filter(|row| native_chat_child(row))
            .map(|row| {
                let participant = crate::text(row, "participant");
                (
                    participant.to_owned(),
                    observations.chat_binding(participant).cloned(),
                )
            });
        let mut accepted_refresh = false;
        let mut native_activity_current = false;
        let mut checkout_changed = false;
        let mut scope_changed = false;
        let mut detail_usage_frame = DetailUsageFrame::Unchanged;
        let mut checked_agy = None;
        let mut incoming_detail_usage = None;
        let (mut report, snapshot_ok) = if let Some(board) = &mut board {
            let (live, ready) = scope_sample.take().unwrap_or_else(|| {
                let ready = board.worker.ready();
                (target.sample(repo, services, interrupted), ready)
            });
            checkout_changed = board.checkout_changed(&live);
            let launch = std::mem::take(&mut requested)
                || (Instant::now() >= deadline && !board.worker.busy());
            let seed = NativeSeed {
                target: target.clone(),
                observations: observations.clone(),
                footer: main_footer.clone(),
                agy: chat.as_ref().and_then(AgyDockCheck::new),
                detail_usage: None,
                detail_usage_binding: selected_chat
                    .as_ref()
                    .and_then(|(_, binding)| binding.as_ref())
                    .filter(|binding| {
                        detail_binding_matches_selection(
                            binding,
                            &nav,
                            before.as_ref().and_then(|rows| rows.get(nav.selected)),
                            &observations,
                        )
                    })
                    .cloned(),
            };
            let (changed, result, native) = board.update_with_native(
                live,
                launch,
                ready,
                Some(seed),
                &mut pending_detail_usage,
            );
            scope_changed = changed;
            accepted_refresh = result.is_some() && board.phase == RefreshPhase::Fresh;
            let rejected_native = native.is_none()
                && result.is_some()
                && board
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.main.is_some());
            detail_usage_frame = if rejected_native || !board.phase.retains_detail_usage() {
                DetailUsageFrame::Rejected
            } else if native.is_some() {
                DetailUsageFrame::Observed
            } else {
                DetailUsageFrame::Unchanged
            };
            native_activity_current = board
                .phase
                .accepts_native_activity(native.is_some(), changed || checkout_changed);
            #[cfg(target_os = "linux")]
            if changed || checkout_changed || rejected_native || !board.phase.retains_detail_usage()
            {
                agy_first_child_wake.replace(None);
                codex_roster_wake.replace(None);
            }
            let mut native_projection = None;
            if let Some(reading) = native {
                #[cfg(target_os = "linux")]
                {
                    agy_first_child_wake.replace(reading.observations.agy_first_child_watch());
                    codex_roster_wake.replace(reading.observations.codex_roster_watch());
                }
                native_projection = Some(reading.projection);
                if accepted_refresh {
                    checked_agy = reading.agy;
                }
                // A queued same-scope refresh restricts actions, while this
                // completed and generation-checked reading can still paint.
                incoming_detail_usage = reading.detail_usage_evidence;
                observations = reading.observations;
                main_footer = reading.footer;
            } else if rejected_native {
                observations.invalidate_readings();
                main_footer = MainFooter::default();
                if chat.as_ref().is_some_and(|dock| dock.chat.is_some()) {
                    chat = None;
                    nav.notice = Some("Transcript unavailable".into());
                }
                if let Some(detail) = &mut weekly.detail {
                    detail.history.clear();
                }
            }
            if checkout_changed && let Some(Value::Object(fields)) = &mut previous {
                fields.remove("board_checkout");
                fields.remove("board_checkout_last_known");
            }
            if changed {
                room = board.scope.as_ref().map_or("", |scope| &scope.room).into();
                read = attention::load(repo, &room);
                selection_changed = if selected_chat.is_some() {
                    SelectionFence::Native
                } else if before.as_ref().is_some_and(|rows| !rows.is_empty()) {
                    SelectionFence::Changed
                } else {
                    SelectionFence::None
                };
                previous = None;
                before = None;
                selected_chat = None;
                chat = None;
                reset_weekly_scope(&mut weekly, true);
                observations = Default::default();
                main_footer = MainFooter::default();
                main_checkout.clear();
                nav.selected = 0;
                nav.offset = 0;
                nav.detail = false;
                nav.folded.clear();
                nav.notice = None;
                switch.cancel_selection(&mut nav);
            }
            let mut report = board.phase.frame_report(result, previous.take(), &room);
            if rejected_native {
                clear_rejected_native(&mut report);
            } else if let Some(projection) = native_projection {
                // A queued same-scope collection limits action authority. The
                // last exact public observation can still refresh the frame.
                replace_native_projection(&mut report, projection);
                if !accepted_refresh {
                    clear_private_weekly(&mut report);
                }
            }
            (report, board.phase == RefreshPhase::Fresh)
        } else {
            let snapshot = commands::snapshot_with(
                session.as_ref().expect("synchronous Watch Session"),
                repo,
                options,
                services,
                interrupted,
            );
            let snapshot_ok = snapshot.as_ref().is_ok_and(|snapshot| {
                snapshot.status == 0 && snapshot.report["collection"]["ok"] == true
            });
            let report = match snapshot {
                Ok(state) => {
                    status = state.status;
                    state.report
                }
                Err(e) => {
                    status = e.code;
                    if let Some(state) = &previous {
                        nav.notice = Some(format!("Refresh failed: {}", e.message));
                        let mut held =
                            oms_panel_core::state::hold_state(state, &json!({"error":e.message}));
                        clear_private_weekly(&mut held);
                        held
                    } else {
                        return Err(e);
                    }
                }
            };
            (report, snapshot_ok)
        };
        if let Some(name) = repo.file_name().and_then(std::ffi::OsStr::to_str) {
            report["project_name"] = json!(name);
        }
        if let Some(board) = &board {
            if !accepted_refresh
                || board
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.main.is_none())
            {
                main_footer.project(
                    board.scope.as_ref(),
                    accepted_refresh,
                    &mut report,
                    Instant::now(),
                    |_| None,
                );
            }
            main_checkout.project(
                board.scope.as_ref(),
                accepted_refresh,
                checkout_changed,
                &mut report,
                Instant::now(),
            );
        }
        let pending = report["collection"]["pending"] == true;
        if board.is_none() && interactive {
            if let Ok(panel) = Panel::load(repo, &room, services, interrupted) {
                panel.observe(&mut observations, &mut report, snapshot_ok);
                report["native_dock"] = json!(panel.docked());
            } else {
                observations.clear_chats();
            }
        }
        // This action is earned by an explicit quota request. Its pane proof
        // and dismissal run independently from passive observation and paint.
        if snapshot_ok
            && let Some(board) = &mut board
            && let Some(scope) = &board.scope
        {
            board
                .quota
                .finish_usage(repo, target.clone(), scope, &mut observations);
        }
        if !observations.quota_pending()
            && nav
                .notice
                .as_deref()
                .is_some_and(|s| s.starts_with(QUOTA_CHECKING))
        {
            nav.notice = None;
        }
        if !pending && !snapshot_ok {
            observations.clear_chats();
        }
        let weekly_phase = WeeklyPhase::from_report(&report, snapshot_ok);
        if weekly_phase == WeeklyPhase::Fresh
            && let Some(detail) = &mut weekly.detail
        {
            detail.history =
                render::weekly_usage_detail(&report, oms_panel_core::timestamp() as f64);
        }
        if let Some(before) = before {
            let after = render::visible(&report, &nav);
            let page = key_page(
                &report,
                &nav,
                last_size,
                interactive.then_some(split.painted_rows),
                &read,
            );
            reconcile_selection_with_page(&before, &after, &mut nav, &mut selection_changed, page);
        }
        if let Some((participant, binding)) = selected_chat
            && observations.chat_binding(&participant) != binding.as_ref()
        {
            selection_changed = SelectionFence::Native;
            nav.detail = false;
        }
        if pending_detail_usage.as_ref().is_some_and(|pending| {
            let visible = render::visible(&report, &nav);
            !detail_binding_matches_selection(
                &pending.binding,
                &nav,
                visible.get(nav.selected),
                &observations,
            )
        }) {
            pending_detail_usage = None;
        }
        reconcile_detail_usage_evidence(
            &mut retained_detail_usage,
            incoming_detail_usage,
            board.as_ref().and_then(|board| board.scope.as_ref()),
            &report,
            &nav,
            &observations,
            detail_usage_frame,
            scope_changed,
        );
        settle_main_dock(&mut chat, &report);
        let owner = board
            .as_ref()
            .and_then(|board| board.scope.as_ref())
            .and_then(|scope| scope.main.as_ref())
            .map(|main| main.owner.clone());
        let owner = owner.as_deref().or_else(|| options.get("owner"));
        let chat_closed = if board.is_some() {
            settle_live_chat_dock_with(&mut chat, &observations, &report, |binding, locator| {
                checked_agy
                    .as_ref()
                    .is_some_and(|check| &check.binding == binding && &check.locator == locator)
            })
        } else {
            settle_live_chat_dock(
                &mut chat,
                &observations,
                &report,
                repo,
                &room,
                owner,
                None,
                session.as_ref(),
                services,
                interrupted,
            )
        };
        if chat_closed {
            nav.notice = Some("Transcript unavailable".into());
        }
        if logging {
            for line in log.lines(&report, &oms_panel_core::now()) {
                writeln!(output, "{}", render::clean(&line, 4096)).map_err(Error::new)?;
            }
        } else {
            let mut size = crate::layout::dimensions();
            if interactive && follow && size != last_size {
                crate::layout::reflow(repo, &room, services, interrupted);
                size = crate::layout::dimensions();
            }
            let display_report = report_for_frame(&report, &nav, retained_detail_usage.as_ref());
            let frame = frame_with_native_activity(
                &display_report,
                &nav,
                size,
                &read,
                colored,
                interactive.then_some(&mut split),
                chat.as_mut(),
                weekly.overlay(weekly_phase),
                native_activity_current,
            );
            provider_rows = frame.provider_rows;
            worker_rows = frame.worker_rows;
            weekly_hit = frame.weekly_usage;
            main_switch_hit = frame.main_switch;
            draw(
                output,
                &frame.lines,
                interactive,
                size.1,
                &mut last_frame,
                size != last_size,
            )?;
            last_size = size;
        }
        output.flush().map_err(Error::new)?;
        ticks += 1;
        previous = Some(report.clone());
        if max_count.is_some_and(|n| ticks >= n)
            || (!follow && terminal.is_none())
            || (logging && !options.has("follow"))
        {
            break;
        }
        let bounded_retry = observations.quota_pending()
            || observations.has_usage_dismissal()
            || chat.as_ref().is_some_and(|dock| dock.chat.is_some());
        deadline = Instant::now()
            + refresh_interval(
                wake.is_some(),
                board
                    .as_ref()
                    .is_some_and(|board| board.phase == RefreshPhase::Fresh),
                bounded_retry,
            );
        loop {
            if interrupted.load(Ordering::Acquire) {
                return Ok((130, vec![], vec![]));
            }
            if Instant::now() >= deadline {
                break;
            }
            if let Some(terminal) = &mut terminal {
                #[cfg(target_os = "linux")]
                let native_fd = [agy_first_child_wake.fd(), codex_roster_wake.fd()];
                #[cfg(not(target_os = "linux"))]
                let native_fd = [None; 2];
                let (keys, woken, _native_ready) = terminal.wait_woken(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(100)),
                    wake.as_ref(),
                    native_fd,
                )?;
                #[cfg(target_os = "linux")]
                let woken = {
                    let idle = board.as_ref().is_some_and(|board| !board.worker.busy());
                    // Settle the current authoritative paint before scheduling
                    // another job from a burst of transcript writes.
                    let now = Instant::now();
                    let agy_woken = agy_first_child_wake.refresh(_native_ready[0], idle, now);
                    let codex_woken = codex_roster_wake.refresh(_native_ready[1], idle, now);
                    woken || agy_woken || codex_woken
                };
                let mut refresh = woken;
                let mut collect_ready = false;
                let mut scope = InputScope {
                    current: board.as_ref().is_none_or(|board| board.scope.is_some()),
                    ..Default::default()
                };
                if let Some(board) = &mut board {
                    collect_ready = scope.current && board.worker.ready();
                    if keys.is_empty() && (collect_ready || woken) {
                        scope.observe(
                            board,
                            target.sample(repo, services, interrupted),
                            &mut report,
                            &mut scope_sample,
                            collect_ready,
                            woken,
                        );
                    }
                }
                let mut changed = false;
                if let Some(outcome) = switch.poll(nav.notice.as_deref(), |pending| {
                    let Some(current) = board.as_ref().and_then(|board| board.scope.as_ref())
                    else {
                        return false;
                    };
                    let Some(main) = pending.scope.main.as_ref() else {
                        return false;
                    };
                    if !same_footer_scope(current, &pending.scope) {
                        return false;
                    }
                    let Some(session) = crate::native_panel::operator_exit_session(
                        repo,
                        &pending.scope.room,
                        main,
                        &pending.target,
                    ) else {
                        return false;
                    };
                    pending.armed
                        || (target
                            .sample(repo, services, interrupted)
                            .is_ok_and(|current| same_footer_scope(&current, &pending.scope))
                            && oms_panel_core::terminals::provider_session_proof(
                                services,
                                repo,
                                &main.session,
                                interrupted,
                            )
                            .is_ok_and(|current| current == session))
                }) {
                    nav.notice = Some(outcome);
                    changed = true;
                    refresh = true;
                }
                let mut detail_repaint = false;
                // A moved dock resizes this pane; paint before the slow snapshot.
                let mut dock_moved = false;
                let mut calls_key: Option<(String, bool, crate::layout::View)> = None;
                let mut calls: Vec<Value> = Vec::new();
                weekly.repaint = false;
                for key in keys {
                    if key == input::Key::Unbound {
                        continue;
                    }
                    if nav.main_switch.is_some() {
                        if key == input::Key::Eof {
                            return Ok((0, vec![], vec![]));
                        }
                        if switch.key(
                            &key,
                            &mut nav,
                            || {
                                (snapshot_ok
                                    && !woken
                                    && !interrupted.load(Ordering::Acquire)
                                    && crate::layout::dimensions() == last_size
                                    && last_size.1 >= 6)
                                    .then(|| target.sample(repo, services, interrupted).ok())
                                    .flatten()
                            },
                            |scope, target| start_main_switch(repo, source, scope, target),
                        ) {
                            changed = true;
                            break;
                        }
                        continue;
                    }
                    if weekly.detail.is_none()
                        && !weekly.repaint
                        && report["native_dock"].is_null()
                        && chat
                            .as_ref()
                            .is_some_and(|dock| !private_chat_hidden(&report, dock))
                        && matches!(
                            key,
                            input::Key::Click(..) | input::Key::Drag(_) | input::Key::Release(_)
                        )
                        && let Some(resized) =
                            split.mouse(&key, last_size, crate::layout::dimensions())
                    {
                        changed |= resized;
                        continue;
                    }
                    if let Some(board) = &board
                        && !scope.sampled
                        && key_checks_scope(
                            &key,
                            nav.editing,
                            weekly.detail.is_some(),
                            !report["native_dock"].is_null(),
                        )
                    {
                        scope.observe(
                            board,
                            target.sample(repo, services, interrupted),
                            &mut report,
                            &mut scope_sample,
                            false,
                            false,
                        );
                    }
                    let action_scope_current = scope.current && (board.is_none() || scope.sampled);
                    match weekly.key(
                        &key,
                        &report,
                        WeeklyPaint {
                            hit: weekly_hit,
                            size: last_size,
                        },
                        crate::layout::dimensions(),
                        (scope.current && !changed && !detail_repaint)
                            .then_some(weekly_phase.awaiting(refresh || collect_ready)),
                        nav.editing,
                    ) {
                        WeeklyKey::Pass => {}
                        WeeklyKey::Handled => continue,
                        WeeklyKey::RefreshBoard => {
                            refresh = true;
                            continue;
                        }
                        WeeklyKey::RefreshQuota => {
                            if action_scope_current {
                                refresh |= request_quota_refresh(
                                    repo,
                                    &room,
                                    services,
                                    interrupted,
                                    &mut observations,
                                    &mut nav,
                                );
                            }
                            continue;
                        }
                    }
                    let direct_detail = !nav.editing && key == input::Key::Char('D');
                    let was_changed = changed;
                    if direct_detail {
                        // This overlay only paints the existing frame. It does
                        // not reconcile coexisting legacy and native docks.
                        detail_repaint = true;
                    } else {
                        changed = true;
                    }
                    if action_scope_current
                        && !nav.editing
                        && !report["native_dock"].is_null()
                        && close_native_key(&key, last_size)
                        && crate::layout::dimensions() == last_size
                    {
                        match change_native_dock(repo, &room, services, interrupted, None, 0, owner)
                        {
                            Ok(docked) => {
                                report["native_dock"] = serde_json::json!(docked);
                                nav.notice = None;
                                dock_moved = true;
                            }
                            Err(e) => nav.notice = Some(e.message),
                        }
                        refresh = true;
                        changed = false;
                        break;
                    }
                    if !nav.editing
                        && matches!(key, input::Key::Escape)
                        && chat
                            .as_ref()
                            .is_some_and(|dock| private_chat_hidden(&report, dock))
                    {
                        chat = None;
                        continue;
                    }
                    if let Some(dock) = &mut chat
                        && !nav.editing
                        && !private_chat_hidden(&report, dock)
                    {
                        match key {
                            input::Key::Escape | input::Key::Char('q')
                                if key == input::Key::Escape || split.painted_rows > 0 =>
                            {
                                chat = None;
                                continue;
                            }
                            _ if split.painted_rows > 0
                                && dock.scroll(&key, split.painted_rows.saturating_sub(2)) =>
                            {
                                continue;
                            }
                            _ => {}
                        }
                    }
                    let want = (nav.filter.clone(), nav.attention_only, nav.view);
                    if calls_key.as_ref() != Some(&want) {
                        calls = render::visible(&report, &nav);
                        calls_key = Some(want);
                    }
                    let painted_hit = match key {
                        input::Key::Click(x, y)
                            if crate::layout::dimensions() == last_size
                                && x > 0
                                && usize::from(x) <= last_size.0 =>
                        {
                            usize::from(y)
                                .checked_sub(1)
                                .and_then(|row| worker_rows.get(&row))
                        }
                        _ => None,
                    };
                    let row_authority =
                        action_scope_current && snapshot_ok && (board.is_none() || !refresh);
                    if route_chat_open(
                        &key,
                        painted_hit,
                        &calls,
                        &mut nav,
                        &mut selection_changed,
                        &mut chat,
                        |call| {
                            if !row_authority {
                                return None;
                            }
                            open_selected_chat_dock(
                                repo,
                                &room,
                                owner,
                                call,
                                &observations,
                                board.as_ref(),
                                session.as_ref(),
                                services,
                                interrupted,
                            )
                        },
                    ) {
                        continue;
                    }
                    if let Some(id) = painted_hit {
                        if let Some(call) = select_painted_call(
                            &report,
                            &calls,
                            id,
                            &mut nav,
                            &mut selection_changed,
                            row_authority,
                        ) {
                            match row_dock(repo, &report, call, services, interrupted) {
                                Ok(dock) => {
                                    chat = Some(dock);
                                }
                                Err(err) => nav.notice = Some(err.message),
                            }
                        } else if nav.detail {
                            changed = was_changed;
                            detail_repaint = true;
                        }
                        continue;
                    }
                    let was_detail = nav.detail;
                    let page = key_page(
                        &report,
                        &nav,
                        last_size,
                        interactive.then_some(split.painted_rows),
                        &read,
                    );
                    let action = if let input::Key::Click(x, y) = key {
                        click_action(
                            x,
                            y,
                            last_size,
                            crate::layout::dimensions(),
                            nav.editing,
                            main_switch_hit,
                            &provider_rows,
                        )
                    } else if !action_scope_current && direct_detail {
                        deny_row_action(&mut nav);
                        navigation::Action::None
                    } else {
                        apply_board_key(
                            &mut nav,
                            &mut selection_changed,
                            &key,
                            &report,
                            &calls,
                            page,
                        )
                    };
                    let action =
                        allow_board_action(&mut nav, action, action_scope_current, row_authority);
                    match action {
                        navigation::Action::Provider(index) => {
                            let provider = crate::orchestrator::PROVIDERS[index];
                            match change_native_dock(
                                repo,
                                &room,
                                services,
                                interrupted,
                                Some(provider),
                                split.rows(last_size.1),
                                owner,
                            ) {
                                Ok(docked) => {
                                    report["native_dock"] = serde_json::json!(docked);
                                    chat = None;
                                    nav.notice = None;
                                    dock_moved = true;
                                    refresh = true;
                                    changed = false;
                                    break;
                                }
                                Err(e) => {
                                    nav.notice = Some(e.message);
                                    refresh = true;
                                }
                            }
                        }
                        navigation::Action::Quit => return Ok((0, vec![], vec![])),
                        navigation::Action::Refresh => refresh = true,
                        navigation::Action::Council => {
                            let council = latest_council(&report);
                            if let Some(council) = council {
                                match worker_dock(repo, &council, services, interrupted) {
                                    Ok(dock) => chat = Some(dock),
                                    Err(err) => nav.notice = Some(err.message),
                                }
                            } else {
                                nav.notice = Some("No council recorded in this room".into());
                            }
                        }
                        navigation::Action::Quota => {
                            refresh |= request_quota_refresh(
                                repo,
                                &room,
                                services,
                                interrupted,
                                &mut observations,
                                &mut nav,
                            );
                        }
                        navigation::Action::ReadAll => {
                            let items = attention::items(&report, &read);
                            match attention::mark(repo, &room, &report, &items) {
                                Ok(()) => read = attention::load(repo, &room),
                                Err(e) => nav.notice = Some(e.message),
                            }
                        }
                        action @ (navigation::Action::Open | navigation::Action::Detail) => {
                            if let Some(call) = calls.get(nav.selected) {
                                mark_opened_row_read(
                                    repo, &room, &report, call, &mut read, &mut nav,
                                );
                                if action == navigation::Action::Open
                                    && open_row_dock(
                                        repo,
                                        &report,
                                        call,
                                        &mut nav,
                                        &mut chat,
                                        services,
                                        interrupted,
                                    )
                                {
                                    continue;
                                }
                                let task = crate::text(call, "task_id");
                                if !task.is_empty() {
                                    match oms_panel_core::results::results(
                                        repo,
                                        Some(task),
                                        services,
                                        interrupted,
                                    ) {
                                        Ok(results) => {
                                            crate::reader::page(
                                                repo,
                                                &results,
                                                terminal,
                                                interrupted,
                                            )?;
                                            nav.detail = false;
                                            last_frame.clear();
                                            if board.is_some() {
                                                refresh = true;
                                                break;
                                            }
                                        }
                                        Err(e) => nav.notice = Some(e.message),
                                    }
                                }
                            }
                        }
                        navigation::Action::Results => {
                            let results = oms_panel_core::results::results(
                                repo,
                                None,
                                services,
                                interrupted,
                            )?;
                            crate::reader::page(repo, &results, terminal, interrupted)?;
                            last_frame.clear();
                            if board.is_some() {
                                refresh = true;
                                break;
                            }
                        }
                        navigation::Action::Chats => {
                            let catalog = commands::query(
                                services,
                                NativeCommand::Room,
                                repo,
                                &["show", "--id", &room, "--json"],
                                interrupted,
                            )?;
                            nav.notice = Some(format_chats_notice(&room, &catalog));
                        }
                        navigation::Action::SwitchMain => {
                            let shown = board.as_ref().and_then(|board| board.scope.as_ref());
                            if last_size.1 < 6 {
                                nav.notice = Some("Enlarge the Board to switch Main".into());
                            } else if let Err(error) = switch.open(shown, &report, source, &mut nav)
                            {
                                nav.notice = Some(error.message);
                            } else {
                                chat = None;
                            }
                            break;
                        }
                        navigation::Action::Login => {
                            let session_hint = board
                                .as_ref()
                                .and_then(|board| board.scope.as_ref())
                                .and_then(|scope| scope.main.as_ref())
                                .map(|main| main.session.as_str());
                            nav.notice = Some(open_login_window(
                                repo,
                                source,
                                session_hint,
                                target.caller.as_deref(),
                                services,
                                interrupted,
                            ));
                        }
                        navigation::Action::Controls => {
                            let mut counts = Vec::new();
                            let mut first_error = None;
                            for (kind, label) in [
                                ("terminals", "terminals"),
                                ("jobs", "jobs"),
                                ("workspaces", "workspaces"),
                            ] {
                                match oms_panel_core::terminals::PrivateStore::open(
                                    repo, kind, false, false,
                                )
                                .and_then(|s| s.list())
                                {
                                    Ok(rows) => counts.push(format!("{}: {}", label, rows.len())),
                                    Err(e) => {
                                        if first_error.is_none() {
                                            first_error = Some(e.message);
                                        }
                                    }
                                }
                            }
                            nav.notice = Some(
                                if let Some(err) = first_error
                                    && counts.is_empty()
                                {
                                    err
                                } else {
                                    format!("Managed — {}", counts.join(" · "))
                                },
                            );
                        }
                        navigation::Action::None => {}
                    }
                    card_replaces_main_dock(&nav, &mut chat);
                    if direct_detail
                        && !was_detail
                        && nav.detail
                        && row_authority
                        && let Some(req) = board.as_ref().and_then(|board| {
                            detail_usage_request(
                                board.scope.as_ref(),
                                &report,
                                calls.get(nav.selected),
                                &observations,
                            )
                        })
                    {
                        pending_detail_usage = Some(req);
                        refresh = true;
                    }
                }
                if scope.changed {
                    pending_detail_usage = None;
                    retained_detail_usage = None;
                } else if pending_detail_usage.is_some() || retained_detail_usage.is_some() {
                    let current_visible;
                    let selected_call = if calls_key.as_ref()
                        == Some(&(nav.filter.clone(), nav.attention_only, nav.view))
                    {
                        calls.get(nav.selected)
                    } else {
                        current_visible = render::visible(&report, &nav);
                        current_visible.get(nav.selected)
                    };
                    if pending_detail_usage.as_ref().is_some_and(|pending| {
                        !detail_binding_matches_selection(
                            &pending.binding,
                            &nav,
                            selected_call,
                            &observations,
                        )
                    }) {
                        pending_detail_usage = None;
                    }
                    if retained_detail_usage.as_ref().is_some_and(|retained| {
                        !detail_binding_matches_selection(
                            &retained.binding,
                            &nav,
                            selected_call,
                            &observations,
                        )
                    }) {
                        retained_detail_usage = None;
                    }
                }
                // Opening an owned native transcript uses the shorter revalidation
                // cadence even when this frame began with the event-backed deadline.
                if chat.as_ref().is_some_and(|dock| dock.chat.is_some()) {
                    deadline = deadline.min(Instant::now() + REFRESH_INTERVAL);
                }
                if changed
                    && (board.is_none() || scope.sampled)
                    && scope.current
                    && snapshot_ok
                    && !refresh
                    && chat.as_ref().is_some_and(|dock| dock.chat.is_none())
                    && !report["native_dock"].is_null()
                {
                    match change_native_dock(repo, &room, services, interrupted, None, 0, owner) {
                        Ok(docked) => {
                            report["native_dock"] = serde_json::json!(docked);
                            dock_moved = true;
                            refresh = true;
                            changed = false;
                        }
                        Err(e) => nav.notice = Some(e.message),
                    }
                }
                let recollect = refresh || collect_ready || scope.changed;
                // Once invalidated, later redraws of this report keep the pending cue.
                native_activity_current &= scope.accepts_native_activity_redraw(recollect);
                if recollect {
                    clear_private_weekly(&mut report);
                    if let Some(observation) = report.get_mut("main_model_observation") {
                        retain_main_model(observation);
                    }
                }
                if dock_moved {
                    if follow {
                        crate::layout::reflow(repo, &room, services, interrupted);
                    }
                    let size = pane_size(repo, services, interrupted);
                    let display_report =
                        report_for_frame(&report, &nav, retained_detail_usage.as_ref());
                    let frame = frame(
                        &display_report,
                        &nav,
                        size,
                        &read,
                        colored,
                        interactive.then_some(&mut split),
                        chat.as_mut(),
                        weekly.overlay(weekly_phase.awaiting(refresh || collect_ready)),
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
                    weekly_hit = frame.weekly_usage;
                    main_switch_hit = frame.main_switch;
                    draw(
                        output,
                        &frame.lines,
                        interactive,
                        size.1,
                        &mut last_frame,
                        true,
                    )?;
                    output.flush().map_err(Error::new)?;
                    last_size = size;
                }
                if scope.checkout_repaint {
                    main_checkout.clear();
                }
                if (changed
                    || weekly.repaint
                    || ((detail_repaint || scope.checkout_repaint) && !dock_moved))
                    && !scope.changed
                {
                    let display_report =
                        report_for_frame(&report, &nav, retained_detail_usage.as_ref());
                    let frame = frame_with_native_activity(
                        &display_report,
                        &nav,
                        last_size,
                        &read,
                        colored,
                        interactive.then_some(&mut split),
                        chat.as_mut(),
                        weekly.overlay(weekly_phase.awaiting(refresh || collect_ready)),
                        native_activity_current,
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
                    weekly_hit = frame.weekly_usage;
                    main_switch_hit = frame.main_switch;
                    draw(
                        output,
                        &frame.lines,
                        interactive,
                        last_size.1,
                        &mut last_frame,
                        false,
                    )?;
                    output.flush().map_err(Error::new)?;
                }
                if recollect {
                    requested = refresh;
                    break;
                }
                let size = crate::layout::dimensions();
                if size != last_size {
                    if follow && scope.current {
                        crate::layout::reflow(repo, &room, services, interrupted);
                    }
                    let size = crate::layout::dimensions();
                    last_size = size;
                    let display_report =
                        report_for_frame(&report, &nav, retained_detail_usage.as_ref());
                    let frame = frame_with_native_activity(
                        &display_report,
                        &nav,
                        size,
                        &read,
                        colored,
                        interactive.then_some(&mut split),
                        chat.as_mut(),
                        weekly.overlay(weekly_phase),
                        native_activity_current,
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
                    weekly_hit = frame.weekly_usage;
                    main_switch_hit = frame.main_switch;
                    draw(
                        output,
                        &frame.lines,
                        interactive,
                        size.1,
                        &mut last_frame,
                        true,
                    )?;
                    output.flush().map_err(Error::new)?;
                }
            } else if crate::wakeup::wait(
                wake.as_ref(),
                deadline.saturating_duration_since(Instant::now()),
                interrupted,
            ) {
                break;
            }
        }
    }
    Ok((status, vec![], vec![]))
}

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const EVENT_BACKED_FALLBACK: Duration = Duration::from_secs(30);

fn refresh_interval(event_backed: bool, fresh: bool, bounded_retry: bool) -> Duration {
    if event_backed && fresh && !bounded_retry {
        EVENT_BACKED_FALLBACK
    } else {
        REFRESH_INTERVAL
    }
}

// A key can enter or leave the filter or an overlay partway through a read.
// Classify it at dispatch rather than guessing from the first key in the batch.
fn key_checks_scope(key: &input::Key, editing: bool, weekly_open: bool, docked: bool) -> bool {
    use input::Key;
    if weekly_open {
        return !editing && *key == Key::Char('U');
    }
    match key {
        Key::Click(..) => true,
        Key::Escape => !editing && docked,
        Key::Enter
        | Key::Char('1' | '2' | '3' | 'U' | 'w' | 'D' | 'c' | 'f' | 'r' | 'R' | 'h' | 's') => {
            !editing
        }
        _ => false,
    }
}

#[derive(Default)]
struct InputScope {
    current: bool,
    sampled: bool,
    changed: bool,
    checkout_repaint: bool,
}

impl InputScope {
    fn accepts_native_activity_redraw(&self, recollect: bool) -> bool {
        self.current && !self.checkout_repaint && !recollect
    }

    fn observe(
        &mut self,
        board: &InteractiveRefresh,
        live: Result<BoardScope>,
        report: &mut Value,
        sample: &mut Option<(Result<BoardScope>, bool)>,
        ready: bool,
        woken: bool,
    ) {
        self.sampled = true;
        self.changed = !board.matches(&live);
        self.current = !self.changed && live.is_ok();
        let checkout_changed = board.checkout_changed(&live);
        if checkout_changed && let Some(fields) = report.as_object_mut() {
            fields.remove("board_checkout");
            fields.remove("board_checkout_last_known");
            clear_footer_model(fields);
            self.checkout_repaint = true;
        }
        if ready || woken || self.changed || sample.is_some() {
            *sample = Some((live, ready));
        }
    }
}
// Only exact, live tags from the Board's own window select a managed Room/Main.
const WATCH_PANES: &str = "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t#{@oms_panel_owner}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_panel_repo}\t.\t#{?#{m/r:[[:cntrl:]],#{pane_start_path}},,#{pane_start_path}}";

#[derive(Clone, Debug, PartialEq, Eq)]
struct BoardScope {
    key: RefreshKey,
    room: String,
    main: Option<LiveMain>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveMain {
    pub(crate) attempt: String,
    pub(crate) member: String,
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) window: String,
    pub(crate) pane: String,
    pub(crate) pid: u32,
    pub(crate) birth: String,
}

const CLAUDE_EXIT_INSTRUCTION: &str = "Main switch ready · Exit Claude normally in the Main pane";

struct PendingOperatorExit {
    scope: BoardScope,
    target: crate::native_panel::MainSwitchTarget,
    next_probe: Instant,
    armed: bool,
}

#[derive(Default)]
struct MainSwitch {
    selection: Option<(BoardScope, Vec<crate::native_panel::MainSwitchTarget>)>,
    running: Option<Receiver<Result<()>>>,
    operator_exit: Option<PendingOperatorExit>,
}

fn legacy_main_switch_source(report: &Value, main: &LiveMain, attempt: &Value) -> bool {
    let room = &report["room"];
    let refs = &attempt["refs"];
    if stale_report(report)
        || report["collection"]["pending"] == true
        || room["closed"] != false
        || !room["successor"].is_null()
        || attempt["attempt_id"] != main.attempt
        || attempt["tool"] != "panel-main"
        || refs["panel_role"] != "main"
        || refs["panel_room_id"] != room["id"]
        || !oms_command_room::main_owner_matches(crate::text(refs, "panel_harness"), &main.owner)
        || [
            "panel_host",
            "panel_wrapper_pid",
            "panel_wrapper_identity",
            "panel_native_pid",
            "panel_native_identity",
        ]
        .iter()
        .any(|key| !refs[*key].is_null())
    {
        return false;
    }
    let mut mains = crate::rows(&room["participants"]).iter().filter(|row| {
        row["participant"] == main.member || (row["role"] == "main" && row["joined"] == true)
    });
    mains.next().is_some_and(|row| {
        row["participant"] == main.member
            && row["provider"] == main.owner
            && row["role"] == "main"
            && row["joined"] == true
            && (row["parent"].is_null() || row["parent"] == "")
    }) && mains.next().is_none()
}

impl MainSwitch {
    fn open(
        &mut self,
        scope: Option<&BoardScope>,
        report: &Value,
        source: &Path,
        nav: &mut navigation::Navigation,
    ) -> Result<()> {
        crate::parent_only()?;
        self.open_with(scope, report, nav, |current| {
            crate::native_panel::main_switch_targets(source, current)
        })
    }

    fn open_with(
        &mut self,
        scope: Option<&BoardScope>,
        report: &Value,
        nav: &mut navigation::Navigation,
        targets: impl FnOnce(&str) -> Result<Vec<crate::native_panel::MainSwitchTarget>>,
    ) -> Result<()> {
        crate::require(
            self.running.is_none(),
            "A Main switch is already in progress",
        )?;
        let scope = scope
            .filter(|scope| report["room"]["id"] == scope.room)
            .ok_or_else(|| Error::new("Main switch requires a verified managed Board"))?;
        let main = scope
            .main
            .as_ref()
            .ok_or_else(|| Error::new("No current Main verified"))?;
        let attempt = &report["room_attempts"][&main.attempt];
        let current = attempt["provider"] == main.owner
            && attempt["terminal"] == false
            && attempt["state"] == "working"
            && oms_panel_core::state::main_participant(attempt) == main.member;
        if !current {
            let live_state = (attempt["attempt_id"] == main.attempt
                && attempt["provider"] == main.owner
                && attempt["terminal"] == false
                && oms_panel_core::state::main_participant(attempt) == main.member)
                .then(|| attempt["state"].as_str())
                .flatten()
                .filter(|state| {
                    *state != "working" && oms_panel_core::state::LIVE_STATES.contains(state)
                });
            if let Some(state) = live_state {
                return Err(Error::new(format!(
                    "Current Main is {state}; wait until it is working to switch"
                )));
            }
            return Err(Error::new(
                "Current Main custody changed or is no longer live; refresh the Board before switching",
            ));
        }
        if legacy_main_switch_source(report, main, attempt) {
            return Err(Error::new(
                "This Main predates managed switching. Hand off work, exit normally, then launch a new managed Main.",
            ));
        }
        match attempt["refs"]["panel_host"].as_str() {
            Some("tmux") => {}
            Some("inline") => {
                return Err(Error::new(
                    "Current Main is running inline, not in a managed tmux pane. Hand off work and exit normally before launching a managed tmux Main.",
                ));
            }
            _ => {
                return Err(Error::new(
                    "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
                ));
            }
        }
        if attempt["refs"]["panel_native_scheduling"] == "campaign" {
            return Err(Error::new(
                "Main swap is unavailable during a managed campaign",
            ));
        }
        let current = attempt["refs"]["panel_harness"].as_str();
        if current == Some("agy") && main.owner == "antigravity" {
            return Err(Error::new(
                "safe automatic exit currently requires a Codex Main composer; hand off work and exit the AGY Main normally before launching a replacement",
            ));
        }
        crate::native_panel::check_outgoing_main_swap_harness(current)?;
        let current = current.unwrap_or_default();
        if current == "claude" {
            let refs = &attempt["refs"];
            crate::require(
                !stale_report(report)
                    && report["collection"]["pending"] != true
                    && report["room"]["closed"] == false
                    && report["room"]["successor"].is_null()
                    && attempt["attempt_id"] == main.attempt
                    && attempt["tool"] == "panel-main"
                    && main.owner == "claude"
                    && refs["panel_role"] == "main"
                    && refs["panel_room_id"] == scope.room
                    && refs["panel_wrapper_identity"] == main.birth
                    && crate::text(refs, "panel_wrapper_pid").parse::<u32>().ok() == Some(main.pid)
                    && crate::text(refs, "panel_native_pid")
                        .parse::<u32>()
                        .is_ok_and(|pid| pid > 0 && pid != main.pid)
                    && !crate::text(refs, "panel_native_identity").is_empty(),
                "Current Claude Main lacks verified managed custody; refresh the Board before switching",
            )?;
            crate::require(
                refs[crate::native_panel::CLAUDE_OPERATOR_EXIT_REF]
                    == crate::native_panel::CLAUDE_OPERATOR_EXIT_VERSION,
                crate::native_panel::CLAUDE_OPERATOR_EXIT_UPGRADE,
            )?;
        }
        let targets = targets(current)?;
        nav.main_switch = Some(navigation::MainSwitchMenu {
            current: current.into(),
            choices: targets
                .iter()
                .map(|target| format!("{} · {}", target.harness, target.model))
                .collect(),
            selected: None,
        });
        nav.notice = None;
        self.selection = Some((scope.clone(), targets));
        Ok(())
    }

    fn cancel_selection(&mut self, nav: &mut navigation::Navigation) {
        self.selection = None;
        nav.main_switch = None;
    }

    /// Process one modal step per paint so a key batch cannot skip confirmation.
    fn key(
        &mut self,
        key: &input::Key,
        nav: &mut navigation::Navigation,
        current: impl FnOnce() -> Option<BoardScope>,
        submit: impl FnOnce(
            &BoardScope,
            &crate::native_panel::MainSwitchTarget,
        ) -> Result<Receiver<Result<()>>>,
    ) -> bool {
        let Some(menu) = &mut nav.main_switch else {
            return false;
        };
        match key {
            input::Key::Escape | input::Key::Char('q' | 'n') => {
                self.cancel_selection(nav);
                true
            }
            input::Key::Char(number @ '1'..='9') if menu.selected.is_none() => {
                let index = *number as usize - '1' as usize;
                if index >= menu.choices.len() {
                    return false;
                }
                menu.selected = Some(index);
                true
            }
            input::Key::Char('y' | 'Y') if menu.selected.is_some() => {
                let selected = menu.selected.unwrap();
                let assisted_exit = menu.current == "claude";
                let selection = self.selection.take();
                nav.main_switch = None;
                let result = (|| {
                    let (expected, targets) =
                        selection.ok_or_else(|| Error::new("Main switch selection expired"))?;
                    crate::require(
                        current().is_some_and(|scope| same_footer_scope(&scope, &expected)),
                        "Main changed; reopen the switch menu",
                    )?;
                    let target = targets
                        .get(selected)
                        .ok_or_else(|| Error::new("Main switch selection expired"))?;
                    self.running = Some(submit(&expected, target)?);
                    if assisted_exit {
                        self.operator_exit = Some(PendingOperatorExit {
                            scope: expected,
                            target: target.clone(),
                            next_probe: Instant::now(),
                            armed: false,
                        });
                        Ok(format!(
                            "Preparing Claude Main switch to {}",
                            target.harness
                        ))
                    } else {
                        Ok(format!("Switching Main to {}", target.harness))
                    }
                })();
                nav.notice = Some(result.unwrap_or_else(|error: Error| error.message));
                true
            }
            _ => false,
        }
    }

    fn poll(
        &mut self,
        notice: Option<&str>,
        prove_armed: impl FnOnce(&PendingOperatorExit) -> bool,
    ) -> Option<String> {
        let message = match self.running.as_ref()?.try_recv() {
            Ok(Ok(())) => "Main switch verified".into(),
            Ok(Err(error)) => error.message,
            Err(TryRecvError::Empty) => {
                let pending = self.operator_exit.as_mut()?;
                let now = Instant::now();
                if now < pending.next_probe {
                    return None;
                }
                pending.next_probe = now + Duration::from_millis(500);
                let was_armed = pending.armed;
                pending.armed = prove_armed(pending);
                return match (pending.armed, was_armed) {
                    (true, _) if notice != Some(CLAUDE_EXIT_INSTRUCTION) => {
                        Some(CLAUDE_EXIT_INSTRUCTION.into())
                    }
                    (false, true) => Some("Awaiting verified Main exit".into()),
                    _ => None,
                };
            }
            Err(TryRecvError::Disconnected) => {
                "Main switch outcome unavailable; check the current Room".into()
            }
        };
        self.running = None;
        self.operator_exit = None;
        Some(message)
    }
}

fn start_main_switch(
    repo: &Path,
    source: &Path,
    scope: &BoardScope,
    target: &crate::native_panel::MainSwitchTarget,
) -> Result<Receiver<Result<()>>> {
    let services = oms_panel_core::SubcommandServices::sibling()?;
    let request = main_switch_request(&services.executable, source, repo, scope, target)?;
    let (completed, result) = mpsc::channel();
    // The canonical host owns lifecycle reconciliation once submitted. Closing
    // this display neither retries the transition nor sends a second request.
    std::thread::Builder::new()
        .name("oms-main-switch".into())
        .spawn(move || {
            let result = services
                .execute(&request, &AtomicBool::new(false))
                .and_then(|response| {
                    if response.code == 0
                        && response.cleanup_complete
                        && response.output_complete
                        && !response.limited
                    {
                        Ok(())
                    } else {
                        let detail = oms_panel_core::redact(&response.stderr).unwrap_or_default();
                        let detail = render::clean(detail.trim(), 240);
                        Err(Error::new(if detail.is_empty() {
                            "Main switch outcome unavailable; check the current Room".into()
                        } else {
                            detail
                        }))
                    }
                });
            let _ = completed.send(result);
        })
        .map_err(Error::new)?;
    Ok(result)
}

fn main_switch_request(
    executable: &Path,
    source: &Path,
    repo: &Path,
    scope: &BoardScope,
    target: &crate::native_panel::MainSwitchTarget,
) -> Result<ExecutionRequest> {
    let main = scope
        .main
        .as_ref()
        .ok_or_else(|| Error::new("No current Main verified"))?;
    crate::require(
        executable.is_absolute()
            && rustix::fs::access(executable, rustix::fs::Access::EXEC_OK).is_ok(),
        "Canonical OMS dispatcher unavailable",
    )?;
    let (source, revision) = crate::native_panel::staged_dispatch_args(source, executable)
        .ok_or_else(|| Error::new("Canonical staged OMS dispatcher unavailable"))?;
    let mut argv = vec![
        executable.as_os_str().into(),
        "--oms-dispatch".into(),
        source.into(),
        revision.into(),
        "--".into(),
        "panel".into(),
        "--repo".into(),
        repo.as_os_str().into(),
    ];
    argv.extend(commands::strings(&[
        "--host",
        "tmux",
        "--room",
        &scope.room,
        "--swap-main",
        target.harness,
        "--model",
        &target.model,
    ]));
    let mut request = ExecutionRequest::new(argv, repo);
    request.timeout = Duration::from_secs(420);
    request.output_limit = 64 * 1024;
    // Same explicit attempt override consumed by the command entrypoint.
    request.environment.extend([
        (
            "OMS_PANEL_MAIN_ATTEMPT".into(),
            main.attempt.as_str().into(),
        ),
        ("OMS_PANEL_OWNER".into(), main.owner.as_str().into()),
        ("OMS_ROOM_PARTICIPANT".into(), main.member.as_str().into()),
    ]);
    Ok(request)
}

#[derive(Clone)]
struct BoardTarget {
    room: Option<String>,
    caller: Option<String>,
    managed: bool,
}

const FOOTER_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct FooterReading {
    scope: BoardScope,
    model: Option<String>,
    checked_at: Instant,
}

#[derive(Clone, Default)]
struct MainFooter(Option<FooterReading>);

fn same_footer_scope(before: &BoardScope, after: &BoardScope) -> bool {
    before == after && before.key.same_launch_path(&after.key)
}

fn clear_footer_model(fields: &mut serde_json::Map<String, Value>) {
    if fields
        .get("main_model_observation")
        .is_some_and(|value| matches!(value["source"].as_str(), Some("footer" | "footer-last")))
    {
        fields.remove("main_model_observation");
    }
}

fn retain_main_model(observation: &mut Value) {
    match observation["source"].as_str() {
        Some("rollout") => observation["source"] = json!("rollout-last"),
        Some("footer") => observation["source"] = json!("footer-last"),
        _ => {}
    }
}

impl MainFooter {
    fn project(
        &mut self,
        scope: Option<&BoardScope>,
        accepted_refresh: bool,
        report: &mut Value,
        now: Instant,
        read: impl FnOnce(&BoardScope) -> Option<String>,
    ) {
        let previous = report
            .as_object_mut()
            .and_then(|fields| fields.remove("main_model_observation"));
        let Some((scope, main)) = scope.and_then(|scope| {
            let main = scope.main.as_ref()?;
            (main.owner == "codex" && report["room"]["id"] == scope.room).then_some((scope, main))
        }) else {
            self.0 = None;
            // An unscoped Board can still show its accepted rollout evidence.
            if scope.is_some_and(|scope| scope.main.is_none())
                && let Some(mut rollout) = previous
                && matches!(rollout["source"].as_str(), Some("rollout" | "rollout-last"))
            {
                if !accepted_refresh {
                    retain_main_model(&mut rollout);
                }
                report["main_model_observation"] = rollout;
            }
            return;
        };
        if self
            .0
            .as_ref()
            .is_some_and(|old| !same_footer_scope(&old.scope, scope))
        {
            self.0 = None;
        }
        let fresh = report["collection"]["ok"] == true
            && report["collection"]["pending"] != true
            && report["capacity_stale"] != true;
        let mut rollout = previous.filter(|value| {
            value["attempt"] == main.attempt
                && value["model"]
                    .as_str()
                    .is_some_and(|model| !model.is_empty())
                && matches!(value["source"].as_str(), Some("rollout" | "rollout-last"))
        });
        if rollout
            .as_ref()
            .is_some_and(|value| value["source"] == "rollout")
        {
            if let Some(reading) = &mut self.0 {
                // A fresh rollout supersedes the model without clearing the cooldown.
                reading.model = None;
            }
            if !fresh && let Some(rollout) = &mut rollout {
                retain_main_model(rollout);
            }
        } else {
            let sample = accepted_refresh
                && fresh
                && self.0.as_ref().is_none_or(|reading| {
                    now.saturating_duration_since(reading.checked_at) >= FOOTER_INTERVAL
                });
            if sample {
                self.0 = Some(FooterReading {
                    scope: scope.clone(),
                    model: read(scope),
                    checked_at: now,
                });
            }
            if let Some(reading) = &self.0
                && now.saturating_duration_since(reading.checked_at) < FOOTER_INTERVAL
                && let Some(model) = &reading.model
            {
                let source = if sample { "footer" } else { "footer-last" };
                report["main_model_observation"] =
                    json!({"attempt":main.attempt,"model":model,"source":source});
                return;
            }
        }
        if let Some(rollout) = rollout {
            report["main_model_observation"] = rollout;
        }
    }
}

/// Maximum age for carrying a display-only Main checkout proof across pending
/// refreshes without a fresh verified snapshot.
const CHECKOUT_LAST_KNOWN_MAX_AGE: Duration = Duration::from_secs(30);
/// Maximum consecutive sampling or collection failures tolerated before
/// clearing the display-only Main checkout proof back to unknown.
const CHECKOUT_LAST_KNOWN_MAX_FAILURES: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ValidCheckout {
    attempt: String,
    worktree: String,
    branch: Option<String>,
    detached: bool,
}

impl ValidCheckout {
    fn parse(attempt: &str, value: &Value) -> Option<Self> {
        if attempt.is_empty() || value["attempt"].as_str() != Some(attempt) {
            return None;
        }
        let detached = match value.get("detached") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return None,
        };
        let worktree = value.get("worktree").and_then(Value::as_str)?;
        if worktree.is_empty()
            || worktree.contains(['/', '\\'])
            || render::clean(worktree, 4096) != worktree
        {
            return None;
        }
        let branch = if detached {
            match value.get("branch") {
                None | Some(Value::Null) => None,
                Some(_) => return None,
            }
        } else {
            let branch = value.get("branch").and_then(Value::as_str)?;
            if branch.is_empty()
                || branch.starts_with(['/', '\\'])
                || branch.chars().any(char::is_whitespace)
                || render::clean(branch, 4096) != branch
            {
                return None;
            }
            Some(branch.to_owned())
        };
        Some(Self {
            attempt: attempt.to_owned(),
            worktree: worktree.to_owned(),
            branch,
            detached,
        })
    }

    fn to_last_known_value(&self, room: &str) -> Value {
        json!({
            "room": room,
            "attempt": self.attempt,
            "worktree": self.worktree,
            "branch": self.branch,
            "detached": self.detached,
            "display_only": true,
        })
    }
}

#[derive(Clone, Debug)]
struct CheckoutReading {
    scope: BoardScope,
    checkout: ValidCheckout,
    sampled_at: Instant,
    failures: u8,
}

/// Strictly display-only last-known checkout proof for an unchanged owned Main.
/// Action authority remains gated exclusively on fresh verified snapshots.
#[derive(Clone, Default)]
struct MainCheckout(Option<CheckoutReading>);

impl MainCheckout {
    fn clear(&mut self) {
        self.0 = None;
    }

    fn note_sample_failure(&mut self, now: Instant) {
        if let Some(reading) = &mut self.0 {
            reading.failures = reading.failures.saturating_add(1);
            if reading.failures >= CHECKOUT_LAST_KNOWN_MAX_FAILURES
                || now.saturating_duration_since(reading.sampled_at) >= CHECKOUT_LAST_KNOWN_MAX_AGE
            {
                self.0 = None;
            }
        }
    }

    fn project(
        &mut self,
        scope: Option<&BoardScope>,
        accepted_refresh: bool,
        checkout_changed: bool,
        report: &mut Value,
        now: Instant,
    ) {
        if let Some(fields) = report.as_object_mut() {
            fields.remove("board_checkout_last_known");
        }
        if checkout_changed {
            self.clear();
            return;
        }
        let Some((scope, main)) = scope.and_then(|scope| {
            let main = scope.main.as_ref()?;
            (report["room"]["id"] == scope.room
                && report["room"]["closed"] != true
                && report["room"]["successor"].is_null())
            .then_some((scope, main))
        }) else {
            self.clear();
            return;
        };
        if self
            .0
            .as_ref()
            .is_some_and(|old| !same_footer_scope(&old.scope, scope))
        {
            self.clear();
        }
        let fresh = accepted_refresh
            && report["collection"]["ok"] == true
            && report["collection"]["pending"] != true
            && report["capacity_stale"] != true;
        if fresh {
            let Some(valid) = ValidCheckout::parse(&main.attempt, &report["board_checkout"]) else {
                self.clear();
                return;
            };
            self.0 = Some(CheckoutReading {
                scope: scope.clone(),
                checkout: valid,
                sampled_at: now,
                failures: 0,
            });
            return;
        }
        if report["collection"]["ok"] != true || report["capacity_stale"] == true {
            self.note_sample_failure(now);
            return;
        }
        if report["collection"]["pending"] != true {
            return;
        }
        let Some(reading) = &self.0 else {
            return;
        };
        if reading.failures >= CHECKOUT_LAST_KNOWN_MAX_FAILURES
            || now.saturating_duration_since(reading.sampled_at) >= CHECKOUT_LAST_KNOWN_MAX_AGE
            || !same_footer_scope(&reading.scope, scope)
        {
            self.clear();
            return;
        }
        report["board_checkout_last_known"] = reading.checkout.to_last_known_value(&scope.room);
    }
}

impl BoardTarget {
    fn sample(
        &mut self,
        repo: &Path,
        services: &dyn Services,
        stop: &AtomicBool,
    ) -> Result<BoardScope> {
        self.scope(repo, services, stop).map(|(scope, _)| scope)
    }

    /// The accepted background result establishes the Room/Main. Re-prove
    /// the same live pane and launch-path generation on both sides of capture.
    fn read_footer(
        &mut self,
        repo: &Path,
        services: &dyn Services,
        stop: &AtomicBool,
        accepted: &BoardScope,
    ) -> Option<String> {
        let (before, path) = self.scope(repo, services, stop).ok()?;
        let path = path?;
        let main = before.main.as_ref().filter(|main| main.owner == "codex")?;
        if !same_footer_scope(&before, accepted)
            || !Path::new(&path).is_absolute()
            || path.len() > 4096
            || path.chars().any(char::is_control)
        {
            return None;
        }
        let screen = oms_panel_core::terminals::tmux(
            services,
            repo,
            &commands::strings(&["capture-pane", "-p", "-t", &main.pane]),
            stop,
        )
        .ok()?;
        let (after, _) = self.scope(repo, services, stop).ok()?;
        (same_footer_scope(&before, &after) && !stop.load(Ordering::Acquire))
            .then(|| crate::orchestrator::codex_model_from_chrome(&screen))
            .flatten()
    }

    fn scope(
        &mut self,
        repo: &Path,
        services: &dyn Services,
        stop: &AtomicBool,
    ) -> Result<(BoardScope, Option<String>)> {
        let mut room = self.room.clone();
        let mut main = None;
        let mut launch_path = None;
        if let Some(caller) = self
            .caller
            .as_deref()
            .filter(|pane| commands::valid_target(pane, '%'))
        {
            let raw = oms_panel_core::terminals::tmux(
                services,
                repo,
                &commands::strings(&["list-panes", "-t", caller, "-F", WATCH_PANES]),
                stop,
            )?;
            let rows: Vec<_> = raw
                .lines()
                .map(|line| line.splitn(14, '\t').collect::<Vec<_>>())
                .filter(|row| row.get(12) == Some(&"."))
                .collect();
            let board = rows
                .iter()
                .find(|row| row[2] == caller)
                .ok_or_else(|| Error::new("Board pane unavailable"))?;
            self.managed |= board[5] == "1";
            if self.managed {
                let mut matches = rows.iter().filter(|row| {
                    row[0] == board[0]
                        && row[1] == board[1]
                        && commands::valid_target(row[2], '%')
                        && row[2] != caller
                        && row[4] == "0"
                        && row[5] != "1"
                        && row[6] == board[6]
                        && row[7] == row[2]
                        && row[8] == board[8]
                        && row[9] == board[9]
                        && row[10] == board[10]
                        && row[11] == board[11]
                });
                let row = matches
                    .next()
                    .ok_or_else(|| Error::new("Live Room Main unavailable"))?;
                crate::require(
                    matches.next().is_none()
                        && commands::valid_target(board[0], '$')
                        && commands::valid_target(board[1], '@')
                        && board[3].parse::<u32>().ok() == Some(std::process::id())
                        && board[4] == "0"
                        && board[5] == "1"
                        && board[7] == row[2]
                        && crate::identifier(board[9]).is_ok()
                        && Some(board[11]) == repo.to_str()
                        && crate::orchestrator::PROVIDERS.contains(&row[8]),
                    "Exact Room Board custody unavailable",
                )?;
                let pid = row[3].parse().map_err(Error::new)?;
                let birth = oms_panel_core::terminals::process(pid)?.birth;
                room = Some(row[10].into());
                launch_path = row.get(13).map(|path| (*path).to_owned());
                main = Some(LiveMain {
                    attempt: row[6].into(),
                    member: row[9].into(),
                    owner: row[8].into(),
                    session: row[0].into(),
                    window: row[1].into(),
                    pane: row[2].into(),
                    pid,
                    birth,
                });
            }
        }
        Ok((
            BoardScope {
                key: RefreshKey::new(
                    repo,
                    room.as_deref(),
                    main.as_ref().map(|main| main.attempt.as_str()),
                )?
                .with_member(main.as_ref().map(|main| main.member.as_str()))?
                .with_launch_path(launch_path.clone()),
                room: room.unwrap_or_default(),
                main,
            },
            launch_path,
        ))
    }
}

#[derive(Clone, PartialEq, Eq)]
struct DetailUsageRequest {
    scope: BoardScope,
    binding: ChatBinding,
}

#[derive(Clone, PartialEq, Eq)]
struct DetailUsageEvidence {
    scope: BoardScope,
    binding: ChatBinding,
    response: Value,
    observed_model: Option<String>,
    claude_version: Option<crate::claude_child_chat::ClaudeChildVersion>,
    requires_group: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailUsageFrame {
    Unchanged,
    Observed,
    Rejected,
}

fn report_has_live_main(report: &Value, main: &LiveMain) -> bool {
    let attempt = &report["room_attempts"][&main.attempt];
    let member = crate::rows(&report["room"]["participants"])
        .iter()
        .find(|row| row["participant"] == main.member);
    attempt["provider"] == main.owner
        && oms_panel_core::state::main_participant(attempt) == main.member
        && member.is_some_and(|row| {
            row["terminal"] != true
                && !row["state"]
                    .as_str()
                    .is_some_and(|state| oms_panel_core::state::TERMINAL_STATES.contains(&state))
        })
}

fn detail_usage_request(
    scope: Option<&BoardScope>,
    report: &Value,
    call: Option<&Value>,
    observations: &Observations,
) -> Option<DetailUsageRequest> {
    let scope = scope?;
    let main = scope.main.as_ref()?;
    if report["room"]["id"] != scope.room || !report_has_live_main(report, main) {
        return None;
    }
    let call = call?;
    let participant = crate::text(call, "participant");
    let binding = observations.chat_binding(participant)?;
    let (provider, _) = binding.detail_target()?;
    if !detail_usage_missing(call, provider)
        || (provider == "claude" && observations.claude_version(participant).is_none())
    {
        return None;
    }
    binding
        .matches_detail_scope(
            &scope.room,
            &main.attempt,
            &main.owner,
            &main.pane,
            main.pid,
            &main.birth,
            participant,
        )
        .then(|| DetailUsageRequest {
            scope: scope.clone(),
            binding: binding.clone(),
        })
}

fn detail_usage_missing(call: &Value, provider: &str) -> bool {
    call["native_child"] == true
        && call["provider"] == provider
        && !call["native_response_usage"]["response"].is_object()
}

fn detail_binding_matches_selection(
    binding: &ChatBinding,
    nav: &navigation::Navigation,
    call: Option<&Value>,
    observations: &Observations,
) -> bool {
    if !nav.detail {
        return false;
    }
    let Some(call) = call else {
        return false;
    };
    let Some((provider, participant)) = binding.detail_target() else {
        return false;
    };
    detail_usage_missing(call, provider)
        && call["participant"] == participant.as_ref()
        && observations.chat_binding(participant.as_ref()) == Some(binding)
}

#[allow(clippy::too_many_arguments)]
fn reconcile_detail_usage_evidence(
    retained: &mut Option<DetailUsageEvidence>,
    incoming: Option<DetailUsageEvidence>,
    scope: Option<&BoardScope>,
    report: &Value,
    nav: &navigation::Navigation,
    observations: &Observations,
    frame: DetailUsageFrame,
    scope_changed: bool,
) {
    if retained.is_none() && incoming.is_none() {
        return;
    }
    if scope_changed || frame == DetailUsageFrame::Rejected || !nav.detail {
        *retained = None;
        return;
    }
    let Some(scope) = scope else {
        *retained = None;
        return;
    };
    let Some(main) = scope.main.as_ref() else {
        *retained = None;
        return;
    };
    if report["room"]["id"] != scope.room || !report_has_live_main(report, main) {
        *retained = None;
        return;
    }
    let visible = render::visible(report, nav);
    let Some(call) = visible.get(nav.selected) else {
        *retained = None;
        return;
    };
    let participant = crate::text(call, "participant");
    let matches = |ev: &DetailUsageEvidence| {
        let Some((provider, _)) = ev.binding.detail_target() else {
            return false;
        };
        detail_usage_missing(call, provider)
            && (provider != "claude"
                || ev.claude_version.is_some_and(|version| {
                    observations.claude_detail_version(&ev.binding) == Some(version)
                }))
            && same_footer_scope(&ev.scope, scope)
            && ev.binding.matches_detail_scope(
                &scope.room,
                &main.attempt,
                &main.owner,
                &main.pane,
                main.pid,
                &main.birth,
                participant,
            )
            && observations.chat_binding(participant) == Some(&ev.binding)
    };
    if frame == DetailUsageFrame::Observed
        && let Some(ev) = incoming
        && matches(&ev)
    {
        *retained = Some(ev);
        return;
    }
    if retained.as_ref().is_some_and(|ev| {
        !matches(ev) || (ev.requires_group && frame == DetailUsageFrame::Observed)
    }) {
        *retained = None;
    }
}

fn report_for_frame<'a>(
    report: &'a Value,
    nav: &navigation::Navigation,
    retained: Option<&DetailUsageEvidence>,
) -> std::borrow::Cow<'a, Value> {
    if !nav.detail {
        return std::borrow::Cow::Borrowed(report);
    }
    let Some(ev) = retained else {
        return std::borrow::Cow::Borrowed(report);
    };
    let Some((provider, participant)) = ev.binding.detail_target() else {
        return std::borrow::Cow::Borrowed(report);
    };
    let mut cloned = report.clone();
    if let Some(rows) = cloned["native_children"].as_array_mut() {
        let mut matching = rows.iter_mut().filter(|row| {
            row["native_child"] == true
                && row["provider"] == provider
                && row["participant"] == participant.as_ref()
        });
        if let (Some(row), None) = (matching.next(), matching.next())
            && !row["native_response_usage"]["response"].is_object()
        {
            if let Some(model) = &ev.observed_model
                && row.get("observed_model").is_none()
            {
                row["observed_model"] = json!(model);
            }
            row["native_response_usage"] = json!({
                "scope": "latest_child_response",
                "response": ev.response,
            });
        }
    }
    std::borrow::Cow::Owned(cloned)
}

struct NativeSeed {
    target: BoardTarget,
    observations: Observations,
    footer: MainFooter,
    agy: Option<AgyDockCheck>,
    detail_usage: Option<DetailUsageRequest>,
    detail_usage_binding: Option<ChatBinding>,
}

struct AgyDockCheck {
    binding: ChatBinding,
    locator: crate::agy_child_chat::AgyChildChatLocator,
}

impl AgyDockCheck {
    fn new(dock: &ChatDock) -> Option<Self> {
        Some(Self {
            binding: dock.chat.as_ref()?.1.clone(),
            locator: dock.agy_locator.clone()?,
        })
    }
}

pub(crate) struct NativeRequest {
    scope: BoardScope,
    seed: NativeSeed,
}

pub(crate) struct NativeReading {
    scope: BoardScope,
    generation: ObservationGeneration,
    projection: NativeProjection,
    observations: Observations,
    footer: MainFooter,
    agy: Option<AgyDockCheck>,
    detail_usage_evidence: Option<DetailUsageEvidence>,
}

const NATIVE_FIELDS: [&str; 12] = [
    "native_children",
    "native_dock",
    "provider_sessions",
    "provider_models",
    "provider_activity",
    "provider_delivery",
    "provider_accounts",
    "provider_status",
    "claude_local_weekly",
    "codex_weekly_account",
    "agy_footer_weekly",
    "main_model_observation",
];
type NativeProjection = [Option<Value>; NATIVE_FIELDS.len()];

fn replace_native_projection(report: &mut Value, projection: NativeProjection) -> NativeProjection {
    let fields = report.as_object_mut().expect("Board report is an object");
    let mut projection = projection.into_iter();
    NATIVE_FIELDS.map(|key| match projection.next().flatten() {
        Some(value) => fields.insert(key.into(), value),
        None => fields.remove(key),
    })
}

fn clear_rejected_native(report: &mut Value) {
    let Some(fields) = report.as_object_mut() else {
        return;
    };
    for key in NATIVE_FIELDS {
        if key == "main_model_observation" {
            clear_footer_model(fields);
        } else {
            fields.remove(key);
        }
    }
}

impl NativeRequest {
    fn authorized_detail_usage(&self, report: &Value) -> Option<&DetailUsageRequest> {
        let main = self.scope.main.as_ref()?;
        self.seed.detail_usage.as_ref().filter(|req| {
            same_footer_scope(&req.scope, &self.scope)
                && report["room"]["id"] == self.scope.room
                && report_has_live_main(report, main)
        })
    }

    pub(crate) fn observe(
        &self,
        snapshot: &mut Snapshot,
        sources: &oms_panel_core::metrics::CodexQuotaSources,
        repo: &Path,
        attempts: &oms_panel_core::state::AttemptCache,
        services: &dyn Services,
        stop: &AtomicBool,
    ) -> Option<NativeReading> {
        let mut target = self.seed.target.clone();
        let main = self.scope.main.as_ref()?;
        if snapshot.status != 0
            || snapshot.report["collection"]["ok"] != true
            || !same_footer_scope(&target.sample(repo, services, stop).ok()?, &self.scope)
            || stop.load(Ordering::Acquire)
        {
            return None;
        }
        let room = &self.scope.room;
        let panel = Panel::load(repo, room, services, stop).ok()?;
        let generation = panel.observation_generation().ok()?;
        let original = NATIVE_FIELDS.map(|key| snapshot.report.get(key).cloned());
        let mut observations = self.seed.observations.clone();
        let mut footer = self.seed.footer.clone();
        commands::publish_codex_quota_receipt(repo, Some(&main.attempt), snapshot, sources.clone());
        let report = &mut snapshot.report;
        let detail_usage_req = self.authorized_detail_usage(report);
        let detail_usage_evidence = panel
            .observe_passively(
                &mut observations,
                report,
                &generation,
                DetailUsageSelection {
                    requested: detail_usage_req.map(|req| &req.binding),
                    active: self.seed.detail_usage_binding.as_ref(),
                },
            )
            .and_then(|usage| {
                let req = detail_usage_req?;
                Some(DetailUsageEvidence {
                    scope: self.scope.clone(),
                    binding: req.binding.clone(),
                    response: usage.response,
                    observed_model: usage.observed_model,
                    claude_version: usage.claude_version,
                    requires_group: usage.requires_group,
                })
            });
        report["native_dock"] = json!(panel.docked());
        footer.project(Some(&self.scope), true, report, Instant::now(), |scope| {
            target.read_footer(repo, services, stop, scope)
        });
        let agy = self.seed.agy.as_ref().and_then(|check| {
            let reads = ChatReads::new(services);
            let scoped = Panel::load(repo, room, &reads, stop).ok()?;
            scoped
                .recheck_open_agy_chat(&check.binding, &check.locator, Some(&main.owner), attempts)
                .then(|| AgyDockCheck {
                    binding: check.binding.clone(),
                    locator: check.locator.clone(),
                })
        });
        // Keep the raw Board snapshot separate from the small pane projection.
        let projection = replace_native_projection(report, original);
        #[cfg(all(test, target_os = "linux"))]
        tests::record_provider_counters();
        let same_panes = Panel::load(repo, room, services, stop)
            .and_then(|fresh| fresh.observation_generation())
            .is_ok_and(|after| after == generation);
        if !same_panes
            || stop.load(Ordering::Acquire)
            || !same_footer_scope(&target.sample(repo, services, stop).ok()?, &self.scope)
        {
            return None;
        }
        Some(NativeReading {
            scope: self.scope.clone(),
            generation,
            projection,
            observations,
            footer,
            agy,
            detail_usage_evidence,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshPhase {
    Pending,
    RetryingAfterFailure,
    Fresh,
    Failed(String),
}

fn stale_report(report: &Value) -> bool {
    report["collection"]["ok"] != true || report["capacity_stale"] == true
}

fn clear_private_weekly(report: &mut Value) {
    if let Some(fields) = report.as_object_mut() {
        fields.remove("claude_local_weekly");
        fields.remove("codex_weekly_account");
        fields.remove("agy_footer_weekly");
    }
}

fn reset_weekly_scope(weekly: &mut WeeklyUsage, changed: bool) {
    if changed {
        *weekly = WeeklyUsage::default();
    }
}

impl RefreshPhase {
    fn pending(&self) -> bool {
        matches!(self, Self::Pending | Self::RetryingAfterFailure)
    }

    fn accepts_native_activity(&self, has_reading: bool, scope_changed: bool) -> bool {
        matches!(self, Self::Pending) && has_reading && !scope_changed
    }

    fn retains_detail_usage(&self) -> bool {
        matches!(self, Self::Pending | Self::Fresh)
    }

    fn frame_report(
        &self,
        snapshot: Option<Snapshot>,
        mut previous: Option<Value>,
        room: &str,
    ) -> Value {
        // Render hides a failed read's AGY roster; pending must not relabel it.
        if self.pending()
            && let Some(previous) = previous.as_mut().filter(|report| stale_report(report))
            && let Some(activity) = previous
                .get_mut("provider_activity")
                .and_then(Value::as_object_mut)
        {
            for provider in activity.values_mut().filter_map(Value::as_object_mut) {
                provider.remove("children");
            }
        }
        let mut report = if let Some(snapshot) = snapshot {
            let mut current = snapshot.report;
            // A display-only result has no pane reads of its own. Retain the
            // same-scope observations so its native rows keep their identity.
            if self.pending() {
                let mut previous = match previous {
                    Some(Value::Object(previous)) => previous,
                    _ => Default::default(),
                };
                for key in [
                    "provider_sessions",
                    "provider_models",
                    "provider_activity",
                    "provider_delivery",
                    "provider_accounts",
                    "provider_status",
                    "native_children",
                    "native_dock",
                ] {
                    if let Some(value) = previous.remove(key) {
                        current[key] = value;
                    } else if matches!(key, "provider_accounts" | "provider_status")
                        && let Some(current) = current.as_object_mut()
                    {
                        current.remove(key);
                    }
                }
            }
            current
        } else {
            previous.unwrap_or_else(|| idle_report(room))
        };
        // Only the current frame's native pane proof can add these readings.
        clear_private_weekly(&mut report);
        self.report(report)
    }

    fn report(&self, mut report: Value) -> Value {
        // A pending frame can carry a proof only from the current scope.
        // Erase known failures before relabeling a held report as pending.
        if stale_report(&report)
            || matches!(self, Self::RetryingAfterFailure | Self::Failed(_))
            || report.get("board_write_slots").is_none()
        {
            report["board_write_slots"] = Value::Null;
        }
        match self {
            Self::Fresh => {}
            Self::Pending | Self::RetryingAfterFailure => {
                // The visible data describes the last successful read. The
                // pending flag and private phase prevent acting on it.
                report["collection"] = json!({"ok":true,"pending":true});
                report["capacity_stale"] = false.into();
            }
            Self::Failed(error) => {
                report["collection"] = json!({"ok":false,"error":error});
                report["capacity_stale"] = true.into();
            }
        }
        report
    }
}

// The interactive exit waits only briefly for normal process-group cleanup.
const REFRESH_STOP_BUDGET: Duration = Duration::from_millis(750);

#[derive(Default)]
struct QuotaCompletion(Option<(Arc<AtomicBool>, Receiver<()>)>);

impl QuotaCompletion {
    fn finish_usage(
        &mut self,
        repo: &Path,
        mut target: BoardTarget,
        scope: &BoardScope,
        observations: &mut Observations,
    ) {
        if self
            .0
            .as_ref()
            .is_some_and(|(_, exited)| exited.try_recv() == Err(TryRecvError::Empty))
            || !observations.has_usage_dismissal()
        {
            return;
        }
        let mut action = observations.take_usage_dismissals();
        let repo = repo.to_owned();
        let scope = scope.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let interrupted = stop.clone();
        let (lifetime, exited) = mpsc::channel::<()>();
        if std::thread::Builder::new()
            .name("oms-quota-completion".into())
            .spawn(move || {
                let _lifetime = lifetime;
                if let Ok(services) = oms_panel_core::SubcommandServices::sibling()
                    && target
                        .sample(&repo, &services, &interrupted)
                        .is_ok_and(|live| same_footer_scope(&scope, &live))
                    && let Ok(panel) = Panel::load(&repo, &scope.room, &services, &interrupted)
                {
                    panel.dismiss_observed_usage(&mut action);
                }
            })
            .is_ok()
        {
            self.0 = Some((stop, exited));
        }
    }

    fn cancel(&self) {
        if let Some((stop, _)) = &self.0 {
            stop.store(true, Ordering::Release);
        }
    }

    fn wait_until(&self, deadline: Instant) {
        if let Some((_, exited)) = &self.0 {
            let _ = exited.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

struct InteractiveRefresh {
    worker: BoardRefresh,
    quota: QuotaCompletion,
    scope: Option<BoardScope>,
    phase: RefreshPhase,
    deferred_refresh: bool,
    agy_group_renewal: Option<DetailUsageRequest>,
}

impl InteractiveRefresh {
    fn start(
        workspace_group: Option<oms_panel_core::services::WorkspaceGroupResolver>,
    ) -> Result<Self> {
        Ok(Self {
            worker: BoardRefresh::start(workspace_group)?,
            quota: QuotaCompletion::default(),
            scope: None,
            phase: RefreshPhase::Pending,
            deferred_refresh: false,
            agy_group_renewal: None,
        })
    }

    fn matches(&self, live: &Result<BoardScope>) -> bool {
        self.scope.as_ref() == live.as_ref().ok()
    }

    fn checkout_changed(&self, live: &Result<BoardScope>) -> bool {
        self.scope
            .as_ref()
            .zip(live.as_ref().ok())
            .is_some_and(|(scope, live)| !scope.key.same_launch_path(&live.key))
    }

    fn request(&mut self, scope: &BoardScope, seed: Option<NativeSeed>) {
        let native = seed
            .filter(|_| scope.main.is_some())
            .map(|seed| NativeRequest {
                scope: scope.clone(),
                seed,
            });
        self.phase = match self.worker.request_with_native(scope.key.clone(), native) {
            Ok(_) => RefreshPhase::Pending,
            Err(error) => RefreshPhase::Failed(error.message),
        };
    }

    #[cfg(test)]
    fn update(
        &mut self,
        live: Result<BoardScope>,
        request: bool,
        ready: bool,
    ) -> (bool, Option<Snapshot>) {
        let (changed, snapshot, _) = self.update_with_native(live, request, ready, None, &mut None);
        (changed, snapshot)
    }

    fn update_with_native(
        &mut self,
        live: Result<BoardScope>,
        request: bool,
        ready: bool,
        mut seed: Option<NativeSeed>,
        pending_detail_usage: &mut Option<DetailUsageRequest>,
    ) -> (bool, Option<Snapshot>, Option<NativeReading>) {
        let changed = !self.matches(&live);
        let detail_binding = seed
            .as_ref()
            .and_then(|seed| seed.detail_usage_binding.clone());
        if changed {
            self.quota.cancel();
            self.deferred_refresh = false;
            *pending_detail_usage = None;
            self.agy_group_renewal = None;
            if let Some(seed) = &mut seed {
                seed.observations = Observations::default();
                seed.agy = None;
                seed.detail_usage = None;
                seed.detail_usage_binding = None;
                seed.footer = MainFooter::default();
            }
        }
        let scope = match live {
            Ok(scope) => scope,
            Err(error) => {
                self.scope = None;
                *pending_detail_usage = None;
                self.agy_group_renewal = None;
                self.phase = RefreshPhase::Failed(error.message);
                return (changed, None, None);
            }
        };
        if let Some(pending) = pending_detail_usage.as_ref()
            && !same_footer_scope(&pending.scope, &scope)
        {
            *pending_detail_usage = None;
        }
        if self.agy_group_renewal.as_ref().is_some_and(|detail| {
            detail_binding.as_ref() != Some(&detail.binding)
                || !same_footer_scope(&detail.scope, &scope)
        }) {
            self.agy_group_renewal = None;
        }
        if changed || (request && !self.worker.busy()) {
            let mut dispatched = seed.take();
            if let Some(seed) = &mut dispatched {
                seed.detail_usage = pending_detail_usage
                    .take()
                    .or_else(|| self.agy_group_renewal.clone());
            }
            self.request(&scope, dispatched);
        } else if request && !self.worker.queued() {
            // Keep the current scope's read eligible to paint even if wakes
            // arrive faster than it completes. An already queued scope change
            // covers wakes until its collection starts.
            self.deferred_refresh = true;
        }
        let result = ready.then(|| self.worker.poll(&scope.key)).flatten();
        let follow_up = !self.worker.busy() && std::mem::take(&mut self.deferred_refresh);
        let replied = result.is_some();
        let completed = result.and_then(|result| match result {
            Ok(snapshot)
                if snapshot.status == 0
                    && snapshot.report["collection"]["ok"] == true
                    && scope
                        .main
                        .as_ref()
                        .is_none_or(|main| report_has_live_main(&snapshot.report, main)) =>
            {
                if !follow_up {
                    self.phase = RefreshPhase::Fresh;
                }
                Some(snapshot)
            }
            _ if follow_up => None,
            Ok(_) => {
                self.phase = RefreshPhase::Failed("Live Room Main or collection changed".into());
                None
            }
            Err(error) => {
                self.phase = RefreshPhase::Failed(error.message);
                None
            }
        });
        let native = self.worker.take_native_reading().filter(|reading| {
            completed.is_some()
                && same_footer_scope(&reading.scope, &scope)
                && reading.generation.processes_current()
        });
        let rejected_native = native.is_none() && completed.is_some() && scope.main.is_some();
        if rejected_native {
            self.quota.cancel();
            *pending_detail_usage = None;
            self.agy_group_renewal = None;
        }
        if !changed
            && let Some(evidence) = native
                .as_ref()
                .and_then(|reading| reading.detail_usage_evidence.as_ref())
            && detail_binding.as_ref() == Some(&evidence.binding)
            && same_footer_scope(&evidence.scope, &scope)
        {
            self.agy_group_renewal = evidence.requires_group.then(|| DetailUsageRequest {
                scope: evidence.scope.clone(),
                binding: evidence.binding.clone(),
            });
        }
        if follow_up {
            if let Some(seed) = &mut seed {
                if let Some(reading) = &native {
                    seed.observations = reading.observations.clone();
                    seed.footer = reading.footer.clone();
                } else if rejected_native {
                    seed.observations.invalidate_readings();
                    seed.footer = MainFooter::default();
                    seed.agy = None;
                }
                seed.detail_usage = pending_detail_usage
                    .take()
                    .or_else(|| self.agy_group_renewal.clone());
            }
            self.request(&scope, seed);
            if replied && completed.is_none() && self.phase == RefreshPhase::Pending {
                self.phase = RefreshPhase::RetryingAfterFailure;
            }
        }
        self.scope = Some(scope);
        (changed, completed, native)
    }

    fn chat_cache(&self) -> Option<&oms_panel_core::state::AttemptCache> {
        if self.phase != RefreshPhase::Fresh {
            return None;
        }
        self.worker.idle_attempt_cache(&self.scope.as_ref()?.key)
    }
}

impl Drop for InteractiveRefresh {
    fn drop(&mut self) {
        let deadline = Instant::now() + REFRESH_STOP_BUDGET;
        self.quota.cancel();
        self.worker.stop_until(deadline);
        self.quota.wait_until(deadline);
    }
}

fn idle_report(room: &str) -> Value {
    json!({"room": {"id": room}, "collection": {"ok": true}})
}

fn deny_row_action(nav: &mut navigation::Navigation) {
    nav.detail = false;
    nav.notice = Some("Wait for a current Board snapshot".into());
}

fn allow_board_action(
    nav: &mut navigation::Navigation,
    action: navigation::Action,
    scope_current: bool,
    row_authority: bool,
) -> navigation::Action {
    use navigation::Action;
    let allowed = match action {
        Action::Provider(_) | Action::Chats => scope_current,
        Action::Quota
        | Action::Council
        | Action::Open
        | Action::Detail
        | Action::ReadAll
        | Action::Results
        | Action::SwitchMain => row_authority,
        Action::None | Action::Quit | Action::Refresh | Action::Controls | Action::Login => true,
    };
    if allowed {
        action
    } else {
        deny_row_action(nav);
        Action::None
    }
}

const LOGIN_FALLBACK_COMMAND: &str = "oms connect-services --login-menu";

fn login_command_argv(source: &Path) -> Vec<std::ffi::OsString> {
    let mut cmd = Vec::new();
    if let Some(entry) = crate::executable::resolve(source) {
        if entry.script {
            cmd.push("bash".into());
        }
        cmd.push(entry.path.as_os_str().into());
        if !entry.script
            && let Some((dispatch_source, rev)) =
                crate::native_panel::staged_dispatch_args(source, &entry.path)
        {
            cmd.extend(commands::strings(&[
                "--oms-dispatch",
                &dispatch_source,
                &rev,
                "--",
            ]));
        }
    } else {
        cmd.push("oms".into());
    }
    cmd.extend(commands::strings(&["connect-services", "--login-menu"]));
    cmd
}

fn login_tmux_session(
    repo: &Path,
    session_hint: Option<&str>,
    caller_pane: Option<&str>,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Option<String> {
    if let Some(session) = session_hint.filter(|id| commands::valid_target(id, '$')) {
        return Some(session.to_owned());
    }
    let pane = caller_pane.filter(|id| commands::valid_target(id, '%'))?;
    let session = oms_panel_core::terminals::tmux(
        services,
        repo,
        &commands::strings(&["display-message", "-p", "-t", pane, "#{session_id}"]),
        interrupted,
    )
    .ok()?;
    commands::valid_target(&session, '$').then_some(session)
}

fn open_login_window(
    repo: &Path,
    source: &Path,
    session_hint: Option<&str>,
    caller_pane: Option<&str>,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> String {
    let Some(session) = login_tmux_session(repo, session_hint, caller_pane, services, interrupted)
    else {
        return format!("No tmux session; run: {LOGIN_FALLBACK_COMMAND}");
    };
    let mut argv = commands::strings(&[
        "new-window",
        "-t",
        &format!("{session}:"),
        "-n",
        "login",
        "-c",
    ]);
    argv.push(repo.as_os_str().into());
    argv.extend(login_command_argv(source));
    match oms_panel_core::terminals::tmux(services, repo, &argv, interrupted) {
        Ok(_) => "Sign-in window opened".into(),
        Err(err) => format!(
            "Cannot open sign-in window ({}); run: {LOGIN_FALLBACK_COMMAND}",
            err.message
        ),
    }
}

const QUOTA_CHECKING: &str = "Checking weekly quota";
const QUOTA_UNAVAILABLE: &str = "Quota refresh needs an idle Claude or AGY session";

fn request_quota_refresh(
    repo: &Path,
    room: &str,
    services: &dyn Services,
    interrupted: &AtomicBool,
    observations: &mut crate::orchestrator::Observations,
    nav: &mut navigation::Navigation,
) -> bool {
    let started = crate::orchestrator::Panel::load(repo, room, services, interrupted)
        .map_or(0, |panel| panel.refresh_usage(observations));
    nav.notice = Some(if started > 0 {
        format!("{QUOTA_CHECKING} for {started} provider session(s)")
    } else {
        QUOTA_UNAVAILABLE.into()
    });
    started > 0
}

/// tmux applies a pane's new pty size after join/break-pane returns, so the
/// server's own geometry is the current size; fall back to the pty.
fn pane_size(repo: &Path, services: &dyn Services, stop: &AtomicBool) -> (usize, usize) {
    std::env::var("TMUX_PANE")
        .ok()
        .filter(|pane| commands::valid_target(pane, '%'))
        .and_then(|pane| {
            oms_panel_core::terminals::tmux(
                services,
                repo,
                &commands::strings(&[
                    "display-message",
                    "-p",
                    "-t",
                    &pane,
                    "#{pane_width} #{pane_height}",
                ]),
                stop,
            )
            .ok()
        })
        .and_then(|raw| {
            let (w, h) = raw.split_once(' ')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .filter(|&(w, h): &(usize, usize)| w > 0 && h > 0)
        .unwrap_or_else(crate::layout::dimensions)
}

fn provider_at_row(rows: &[Option<usize>; 3], y: u16) -> Option<usize> {
    let row = usize::from(y).checked_sub(1)?;
    rows.iter().position(|value| *value == Some(row))
}

fn click_action(
    x: u16,
    y: u16,
    last_size: (usize, usize),
    current_size: (usize, usize),
    editing: bool,
    main_switch: Option<render::ActionZone>,
    provider_rows: &[Option<usize>; 3],
) -> navigation::Action {
    if current_size != last_size || x == 0 || usize::from(x) > last_size.0 {
        return navigation::Action::None;
    }
    if !editing && main_switch.is_some_and(|zone| zone.contains(x, y)) {
        return navigation::Action::SwitchMain;
    }
    provider_at_row(provider_rows, y).map_or(navigation::Action::None, navigation::Action::Provider)
}

const CLOSE_NATIVE: &str = "[ Return ]";

fn close_native_key(key: &input::Key, (columns, height): (usize, usize)) -> bool {
    match *key {
        input::Key::Escape => true,
        input::Key::Click(x, y) => {
            usize::from(y) == height
                && (1..=columns.min(CLOSE_NATIVE.len())).contains(&usize::from(x))
        }
        _ => false,
    }
}

fn change_native_dock(
    repo: &Path,
    room: &str,
    services: &dyn Services,
    interrupted: &AtomicBool,
    provider: Option<&str>,
    rows: usize,
    owner: Option<&str>,
) -> Result<Option<&'static str>> {
    crate::orchestrator::Panel::load(repo, room, services, interrupted)?
        .toggle_dock(provider, rows, owner)
}

struct ChatDock {
    title: String,
    history: String,
    back: usize,
    chat: Option<(String, crate::orchestrator::ChatBinding)>,
    agy_locator: Option<crate::agy_child_chat::AgyChildChatLocator>,
    /// The pinned Overall Main identity this summary reads; workers carry none.
    main: Option<(String, String)>,
}

struct WeeklyPaint {
    hit: Option<render::WeeklyUsageHit>,
    size: (usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeeklyPhase {
    Fresh,
    Pending,
    Unavailable,
}

impl WeeklyPhase {
    fn from_report(report: &Value, snapshot_ok: bool) -> Self {
        if stale_report(report) {
            Self::Unavailable
        } else if report["collection"]["pending"] == true {
            Self::Pending
        } else if snapshot_ok {
            Self::Fresh
        } else {
            Self::Unavailable
        }
    }

    fn awaiting(self, refresh: bool) -> Self {
        if refresh && self == Self::Fresh {
            Self::Pending
        } else {
            self
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum WeeklyKey {
    Pass,
    Handled,
    RefreshBoard,
    RefreshQuota,
}

#[derive(Default)]
struct WeeklyUsage {
    detail: Option<ChatDock>,
    repaint: bool,
}

impl WeeklyUsage {
    fn overlay(&mut self, phase: WeeklyPhase) -> Option<(&mut ChatDock, WeeklyPhase)> {
        self.detail.as_mut().map(|detail| (detail, phase))
    }

    fn key(
        &mut self,
        key: &input::Key,
        report: &Value,
        paint: WeeklyPaint,
        actual: (usize, usize),
        phase: Option<WeeklyPhase>,
        editing: bool,
    ) -> WeeklyKey {
        use input::Key;
        if *key == Key::Eof {
            return WeeklyKey::Pass;
        }
        let phase = phase.filter(|_| paint.size == actual && !editing);
        let ready = phase == Some(WeeklyPhase::Fresh);
        if let Some(detail) = &mut self.detail {
            match key {
                Key::Escape | Key::Char('q') => {
                    self.detail = None;
                    self.repaint = true;
                }
                Key::Char('u') if phase == Some(WeeklyPhase::Unavailable) && !self.repaint => {
                    self.repaint = true;
                    return WeeklyKey::RefreshBoard;
                }
                Key::Char('U') if ready && !self.repaint => {
                    self.repaint = true;
                    return WeeklyKey::RefreshQuota;
                }
                _ if ready => {
                    self.repaint |= detail.scroll(key, dock_page(actual.1, actual.0));
                }
                _ => {}
            }
            return WeeklyKey::Handled;
        }
        // Closing the modal does not expose Board targets until the next paint.
        if self.repaint {
            return WeeklyKey::Handled;
        }
        let requested = match *key {
            Key::Char('w') => !editing,
            Key::Click(x, y) => paint.hit.is_some_and(|hit| {
                usize::from(y) == hit.row + 1 && (1..=hit.columns).contains(&usize::from(x))
            }),
            _ => false,
        };
        if requested && phase.is_some() {
            self.detail = Some(ChatDock {
                title: "Weekly usage".into(),
                history: if ready {
                    let now = paint
                        .hit
                        .map_or_else(|| oms_panel_core::timestamp() as f64, |hit| hit.now);
                    render::weekly_usage_detail(report, now)
                } else {
                    String::new()
                },
                back: usize::MAX,
                chat: None,
                agy_locator: None,
                main: None,
            });
            self.repaint = true;
        }
        if requested {
            WeeklyKey::Handled
        } else {
            WeeklyKey::Pass
        }
    }
}

fn private_chat_hidden(report: &Value, dock: &ChatDock) -> bool {
    dock.chat.is_some() && report["collection"]["pending"] == true
}

fn chat_binding_stale(
    chat: &Option<ChatDock>,
    observations: &crate::orchestrator::Observations,
    docked: bool,
) -> bool {
    chat.as_ref().is_some_and(|dock| {
        dock.chat.as_ref().is_some_and(|(participant, binding)| {
            docked || observations.chat_binding(participant) != Some(binding)
        })
    })
}

#[cfg(test)]
fn refresh_chat(
    chat: &mut Option<ChatDock>,
    observations: &crate::orchestrator::Observations,
    docked: bool,
) -> bool {
    let stale = chat_binding_stale(chat, observations, docked)
        || chat.as_ref().is_some_and(|dock| {
            dock.chat.as_ref().is_some_and(|(_, binding)| {
                dock.agy_locator.as_ref().is_some_and(|loc| {
                    !crate::agy_child_chat::agy_locator_still_current_fenced(
                        binding.repo(),
                        loc,
                        || true,
                    )
                })
            })
        });
    if stale {
        *chat = None;
    }
    stale
}

#[cfg(test)]
fn settle_chat_dock(
    chat: &mut Option<ChatDock>,
    observations: &crate::orchestrator::Observations,
    report: &Value,
) -> bool {
    let docked = !report["native_dock"].is_null();
    if report["collection"]["pending"] == true && !docked {
        return false;
    }
    refresh_chat(
        chat,
        observations,
        docked || report["collection"]["ok"] != true,
    )
}

fn with_chat_panel<T>(
    repo: &Path,
    room: &str,
    board: Option<&InteractiveRefresh>,
    session: Option<&commands::Session>,
    services: &dyn Services,
    interrupted: &AtomicBool,
    f: impl FnOnce(&crate::orchestrator::Panel<'_>, &oms_panel_core::state::AttemptCache) -> Option<T>,
) -> Option<T> {
    if let Some(board) = board {
        let attempts = board.chat_cache()?;
        let reads = ChatReads::new(services);
        let panel = crate::orchestrator::Panel::load(repo, room, &reads, interrupted).ok()?;
        f(&panel, attempts)
    } else {
        let session = session?;
        let panel = crate::orchestrator::Panel::load(repo, room, services, interrupted).ok()?;
        f(&panel, session.attempt_cache())
    }
}

fn settle_live_chat_dock_with(
    chat: &mut Option<ChatDock>,
    observations: &crate::orchestrator::Observations,
    report: &Value,
    recheck_agy: impl FnOnce(
        &crate::orchestrator::ChatBinding,
        &crate::agy_child_chat::AgyChildChatLocator,
    ) -> bool,
) -> bool {
    let docked = !report["native_dock"].is_null();
    if report["collection"]["pending"] == true && !docked {
        return false;
    }
    let stale_binding = chat_binding_stale(
        chat,
        observations,
        docked || report["collection"]["ok"] != true,
    );
    let stale_agy = !stale_binding
        && chat.as_ref().is_some_and(|dock| {
            dock.chat
                .as_ref()
                .zip(dock.agy_locator.as_ref())
                .is_some_and(|((_, binding), locator)| !recheck_agy(binding, locator))
        });
    let stale = stale_binding || stale_agy;
    if stale {
        *chat = None;
    }
    stale
}

#[allow(clippy::too_many_arguments)]
fn settle_live_chat_dock(
    chat: &mut Option<ChatDock>,
    observations: &crate::orchestrator::Observations,
    report: &Value,
    repo: &Path,
    room: &str,
    owner: Option<&str>,
    board: Option<&InteractiveRefresh>,
    session: Option<&commands::Session>,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> bool {
    settle_live_chat_dock_with(chat, observations, report, |binding, locator| {
        with_chat_panel(repo, room, board, session, services, interrupted, |p, a| {
            Some(p.recheck_open_agy_chat(binding, locator, owner, a))
        })
        .unwrap_or(false)
    })
}

fn native_chat_child(call: &Value) -> bool {
    call["native_child"] == true
        && matches!(
            call["provider"].as_str(),
            Some("codex" | "claude" | "antigravity")
        )
}

fn route_chat_open(
    key: &input::Key,
    painted_hit: Option<&(String, String)>,
    calls: &[Value],
    nav: &mut navigation::Navigation,
    fence: &mut SelectionFence,
    chat: &mut Option<ChatDock>,
    open: impl FnOnce(&Value) -> Option<ChatDock>,
) -> bool {
    let selected = match key {
        input::Key::Click(..) => {
            let Some(id) = painted_hit else { return false };
            let matched = calls
                .iter()
                .enumerate()
                .find(|(_, call)| &call_identity(call) == id);
            if !id.0.starts_with("native-codex-")
                && !id.0.starts_with("native-claude-")
                && !id.0.starts_with("native-antigravity-")
                && !matched.is_some_and(|(_, call)| native_chat_child(call))
            {
                return false;
            }
            matched.filter(|(_, call)| native_chat_child(call))
        }
        input::Key::Enter if !nav.editing && *fence == SelectionFence::Native => None,
        input::Key::Enter if !nav.editing && *fence == SelectionFence::None => {
            let Some(call) = calls
                .get(nav.selected)
                .filter(|call| native_chat_child(call))
            else {
                return false;
            };
            Some((nav.selected, call))
        }
        _ => return false,
    };
    *fence = SelectionFence::None;
    nav.detail = false;
    *chat = selected.and_then(|(index, call)| {
        nav.selected = index;
        open(call)
    });
    nav.notice = chat.is_none().then(|| "Transcript unavailable".into());
    true
}

fn select_painted_call<'a>(
    report: &Value,
    calls: &'a [Value],
    id: &(String, String),
    nav: &mut navigation::Navigation,
    fence: &mut SelectionFence,
    row_authority: bool,
) -> Option<&'a Value> {
    let (index, call) = calls
        .iter()
        .enumerate()
        .find(|(_, call)| &call_identity(call) == id)?;
    nav.selected = index;
    nav.detail = false;
    if !row_authority {
        deny_row_action(nav);
        return None;
    }
    if call["role"] == "main" {
        *fence = SelectionFence::None;
        // Only the pinned Overall Main opens its summary dock.
        nav.detail = !render::is_overall_main(report, call);
        if nav.detail {
            return None;
        }
    }
    Some(call)
}

fn chat_dock(
    call: &Value,
    observations: &crate::orchestrator::Observations,
    load: impl FnOnce(&crate::orchestrator::ChatBinding) -> Option<String>,
) -> Option<ChatDock> {
    let participant = crate::text(call, "participant");
    let binding = observations.chat_binding(participant)?;
    let history = load(binding)?;
    Some(ChatDock {
        title: chat_title(call),
        history,
        back: 0,
        chat: Some((participant.into(), binding.clone())),
        agy_locator: None,
        main: None,
    })
}

fn open_agy_chat_dock(
    repo: &Path,
    call: &Value,
    observations: &crate::orchestrator::Observations,
    services: &dyn Services,
    interrupted: &AtomicBool,
    open: impl FnOnce(&crate::orchestrator::ChatBinding) -> Option<crate::agy_child_chat::AgyChatOpen>,
) -> Option<ChatDock> {
    let participant = crate::text(call, "participant");
    let has_parent_summary = !crate::text(call, "native_answer").trim().is_empty();
    let fallback = || {
        has_parent_summary
            .then(|| worker_dock(repo, call, services, interrupted).ok())
            .flatten()
    };
    let Some(binding) = observations.chat_binding(participant) else {
        return fallback();
    };
    match open(binding)? {
        #[cfg(target_os = "linux")]
        crate::agy_child_chat::AgyChatOpen::OwnChat { history, locator } => Some(ChatDock {
            title: chat_title(call),
            history,
            back: 0,
            chat: Some((participant.into(), binding.clone())),
            agy_locator: Some(locator),
            main: None,
        }),
        crate::agy_child_chat::AgyChatOpen::Unbound => fallback(),
        crate::agy_child_chat::AgyChatOpen::Unavailable => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn open_selected_chat_dock(
    repo: &Path,
    room: &str,
    owner: Option<&str>,
    call: &Value,
    observations: &crate::orchestrator::Observations,
    board: Option<&InteractiveRefresh>,
    session: Option<&commands::Session>,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Option<ChatDock> {
    if call["native_child"] == true && call["provider"] == "antigravity" {
        return open_agy_chat_dock(repo, call, observations, services, interrupted, |binding| {
            with_chat_panel(
                repo,
                room,
                board,
                session,
                services,
                interrupted,
                |panel, attempts| Some(panel.open_agy_chat(binding, owner, attempts)),
            )
        });
    }

    let open = |reads: &dyn Services,
                attempts: &oms_panel_core::state::AttemptCache,
                binding: &crate::orchestrator::ChatBinding| {
        crate::orchestrator::Panel::load(repo, room, reads, interrupted)
            .ok()?
            .open_chat(binding, owner, attempts)
    };
    let opened = if let Some(board) = board {
        idle_chat_dock(call, observations, board, services, open)
    } else {
        session.and_then(|session| {
            chat_dock(call, observations, |binding| {
                open(services, session.attempt_cache(), binding)
            })
        })
    };
    opened.or_else(|| {
        unbound_summary(call, observations, || {
            worker_dock(repo, call, services, interrupted).ok()
        })
    })
}

/// A Claude or AGY child row with no proven binding still shows what the parent
/// delivered, labelled as such. A bound child whose read failed shows nothing.
fn unbound_summary(
    call: &Value,
    observations: &crate::orchestrator::Observations,
    summary: impl FnOnce() -> Option<ChatDock>,
) -> Option<ChatDock> {
    (native_chat_child(call)
        && matches!(call["provider"].as_str(), Some("claude" | "antigravity"))
        && observations
            .chat_binding(crate::text(call, "participant"))
            .is_none())
    .then(summary)
    .flatten()
}

const PARENT_SUMMARY: &str = "Parent-delivered summary (not this child's own transcript)";

fn idle_chat_dock(
    call: &Value,
    observations: &crate::orchestrator::Observations,
    board: &InteractiveRefresh,
    service: &dyn Services,
    open: impl FnOnce(
        &dyn Services,
        &oms_panel_core::state::AttemptCache,
        &crate::orchestrator::ChatBinding,
    ) -> Option<String>,
) -> Option<ChatDock> {
    let attempts = board.chat_cache()?;
    let reads = ChatReads::new(service);
    chat_dock(call, observations, |binding| {
        open(&reads, attempts, binding)
    })
}

fn chat_title(call: &Value) -> String {
    if call["tool"] == "panel-council" {
        let label = crate::text(&call["refs"], "panel_label").trim();
        return if label.is_empty() {
            "Council".into()
        } else {
            label.into()
        };
    }
    let title = crate::text(call, "title").trim();
    let title = if title.is_empty() {
        crate::text(call, "participant")
    } else {
        title
    };
    format!("{title} · {}", crate::text(call, "role"))
}

fn latest_council(report: &Value) -> Option<Value> {
    let room = crate::text(&report["room"], "id");
    if room.is_empty() {
        return None;
    }
    report["room_attempts"]
        .as_object()?
        .values()
        .filter(|row| {
            let refs = &row["refs"];
            let rooms = [
                crate::text(refs, "panel_room_id"),
                crate::text(refs, "panel_room"),
            ];
            row["tool"] == "panel-council"
                && rooms.contains(&room)
                && rooms.iter().all(|id| id.is_empty() || *id == room)
        })
        .max_by(|a, b| {
            let a_stamp = oms_panel_core::metrics::stamp(&a["updated_at"]);
            let b_stamp = oms_panel_core::metrics::stamp(&b["updated_at"]);
            a_stamp
                .partial_cmp(&b_stamp)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| crate::text(a, "updated_at").cmp(crate::text(b, "updated_at")))
        })
        .cloned()
}

/// The reader dock for a picked Board row: the Overall Main gets its Board
/// summary in two lines, so the four-row default dock shows all of it.
fn row_dock(
    repo: &Path,
    report: &Value,
    call: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<ChatDock> {
    if let Some((title, history)) = render::attention_row_dock(report, call) {
        return Ok(ChatDock {
            title,
            history: oms_panel_core::redact(history.as_bytes())?,
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        });
    }
    if !render::is_overall_main(report, call) {
        return worker_dock(repo, call, services, stop);
    }
    Ok(ChatDock {
        title: "Overall Main".into(),
        history: render::main_summary_lines(call).join("\n"),
        back: 0,
        chat: None,
        agy_locator: None,
        main: Some(call_identity(call)),
    })
}

/// Map a visible board row (worker call, synthetic room message, or synthetic
/// held task) to its `attention::Item::row_id` pair.
fn row_attention_id(call: &Value) -> (&str, &str) {
    let kind = crate::text(call, "attention_kind");
    if matches!(kind, "message" | "task") {
        let id = crate::text(call, "attention_id");
        let id = if id.is_empty() && kind == "task" {
            crate::text(call, "task_id")
        } else {
            id
        };
        (kind, id)
    } else {
        ("call", crate::text(call, "participant"))
    }
}

/// Mark the matching Attention item read when a row is opened or inspected.
fn mark_opened_row_read(
    repo: &Path,
    room: &str,
    report: &Value,
    call: &Value,
    read: &mut std::collections::BTreeMap<String, String>,
    nav: &mut navigation::Navigation,
) {
    let (kind, id) = row_attention_id(call);
    let items: Vec<_> = if id.is_empty() {
        Vec::new()
    } else {
        attention::items(report, read)
            .into_iter()
            .filter(|item| item.row_id.0 == kind && item.row_id.1 == id)
            .collect()
    };
    if let Err(e) = attention::mark(repo, room, report, &items) {
        nav.notice = Some(e.message);
    }
    *read = attention::load(repo, room);
}

/// Enter on a worker or the pinned Main swaps any card for that row's dock.
fn open_row_dock(
    repo: &Path,
    report: &Value,
    call: &Value,
    nav: &mut navigation::Navigation,
    chat: &mut Option<ChatDock>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> bool {
    if call["role"] == "main" && !render::is_overall_main(report, call) {
        return false;
    }
    nav.detail = false;
    match row_dock(repo, report, call, services, stop) {
        Ok(dock) => *chat = Some(dock),
        Err(err) => nav.notice = Some(err.message),
    }
    true
}

/// A key that leaves a card open (D, or `f` without a results page) closes
/// an open Main summary, so the card's one Escape returns to the Board.
/// Worker docks stay.
fn card_replaces_main_dock(nav: &navigation::Navigation, chat: &mut Option<ChatDock>) {
    if nav.detail && chat.as_ref().is_some_and(|dock| dock.main.is_some()) {
        *chat = None;
    }
}

/// Each painted report, pending ones included, rereads an open Main summary
/// from the pinned Main's row, and closes it once another participant or
/// attempt holds that seat.
fn settle_main_dock(chat: &mut Option<ChatDock>, report: &Value) {
    let Some(id) = chat.as_ref().and_then(|dock| dock.main.as_ref()) else {
        return;
    };
    match render::pinned_main(report).filter(|main| call_identity(main) == *id) {
        Some(main) => {
            if let Some(dock) = chat {
                dock.history = render::main_summary_lines(&main).join("\n");
            }
        }
        None => *chat = None,
    }
}

fn worker_dock(
    repo: &Path,
    call: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<ChatDock> {
    let title = chat_title(call);
    let mut history = crate::text(call, "native_answer").to_owned();
    let task = crate::text(call, "task_id");
    let participant = crate::text(call, "participant");
    if call["tool"] == "panel-council" {
        history = council_history(repo, crate::text(&call["refs"], "panel_thread"))?;
    } else if !task.is_empty() && !participant.is_empty() && call["native_child"] != true {
        let results = oms_panel_core::results::results_page(
            repo,
            &oms_panel_core::results::ResultsRequest {
                task_id: Some(task.into()),
                room_participant: Some(participant.into()),
                ..Default::default()
            },
            services,
            stop,
        )?;
        let attempt = crate::text(call, "attempt");
        if let Some(answer) = crate::rows(&results["rows"])
            .iter()
            .flat_map(|task| crate::rows(&task["calls"]))
            .rev()
            .filter(|row| attempt.is_empty() || row["attempt_id"] == attempt)
            .find_map(|row| {
                row["answer"]
                    .as_str()
                    .or_else(|| row["answer_excerpt"].as_str())
            })
        {
            history = oms_panel_core::redact(answer.as_bytes())?;
        }
    }
    let has_parent_summary = !history.trim().is_empty();
    if !has_parent_summary {
        history = if call["tool"] == "panel-council" {
            "No council positions recorded"
        } else {
            "No result recorded for this worker yet"
        }
        .into();
    }
    let wrap_summary = call["native_child"] == true
        && (call["provider"] == "claude"
            || (call["provider"] == "antigravity" && has_parent_summary));
    let (title, history) = if wrap_summary {
        (
            format!("{title} · parent summary"),
            format!("{PARENT_SUMMARY}\n\n{history}"),
        )
    } else {
        (title, history)
    };
    Ok(ChatDock {
        title,
        history,
        back: 0,
        chat: None,
        agy_locator: None,
        main: None,
    })
}

fn council_history(repo: &Path, thread: &str) -> Result<String> {
    let Some(id) = thread
        .strip_prefix("panel-council-")
        .filter(|id| id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return Ok("No linked council log recorded".into());
    };
    let store = oms_panel_core::Store::open(repo)?;
    let raw = store.read(
        Path::new(&format!(".oms/threads/panel-council-{id}.jsonl")),
        2 * 1024 * 1024,
        true,
    )?;
    council_log(&raw, thread)
}

fn council_log(raw: &[u8], thread: &str) -> Result<String> {
    let mut text = Vec::new();
    for line in raw
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let row: Value = serde_json::from_slice(line).map_err(Error::new)?;
        crate::require(
            row["thread"] == thread,
            "council log has a different thread identity",
        )?;
        let content = crate::text(&row, "text");
        if !content.is_empty() {
            text.push(format!(
                "{} · {}\n{}",
                crate::text(&row, "agent"),
                crate::text(&row, "role"),
                content
            ));
        }
    }
    oms_panel_core::redact(text.join("\n\n").as_bytes())
}

impl ChatDock {
    fn scroll(&mut self, key: &input::Key, page: usize) -> bool {
        use input::Key;
        self.back = match key {
            Key::Home => usize::MAX,
            Key::End => 0,
            Key::Up => self.back.saturating_add(1),
            Key::Down => self.back.saturating_sub(1),
            Key::PageUp => self.back.saturating_add(page),
            Key::PageDown => self.back.saturating_sub(page),
            Key::Scroll(n) if *n < 0 => self.back.saturating_add(n.unsigned_abs() as usize),
            Key::Scroll(n) => self.back.saturating_sub(*n as usize),
            _ => return false,
        };
        true
    }
}

#[derive(Default)]
struct DockSplit {
    preferred: Option<usize>,
    drag_size: Option<(usize, usize)>,
    painted_rows: usize,
}

impl DockSplit {
    fn rows(&self, height: usize) -> usize {
        let board_min = if height >= 24 { 16 } else { 7 };
        let max = height.saturating_sub(board_min);
        if max < 4 {
            0
        } else {
            self.preferred
                .unwrap_or_else(|| (height / 3).clamp(4, 32))
                .clamp(4, max)
        }
    }

    fn paint(&mut self, height: usize, visible: bool) -> usize {
        self.painted_rows = if visible { self.rows(height) } else { 0 };
        if self.painted_rows == 0 {
            self.drag_size = None;
        }
        self.painted_rows
    }

    /// A gesture starts on the painted divider. A concurrent terminal resize
    /// invalidates the coordinates until the next mouse press.
    fn mouse(
        &mut self,
        key: &input::Key,
        painted: (usize, usize),
        actual: (usize, usize),
    ) -> Option<bool> {
        use input::Key;
        if self.painted_rows == 0 {
            return None;
        }
        let (columns, height) = actual;
        match *key {
            Key::Click(x, y) => {
                self.drag_size = None;
                if painted == actual
                    && x > 0
                    && usize::from(x) <= columns
                    && usize::from(y) == height - self.painted_rows + 1
                {
                    self.drag_size = Some(actual);
                    return Some(false);
                }
                None
            }
            Key::Drag(y) | Key::Release(y) => {
                let rows = self.rows(height);
                let can_resize = self.drag_size == Some(actual) && painted == actual;
                if matches!(key, Key::Release(_)) || !can_resize {
                    self.drag_size = None;
                }
                if !can_resize || rows == 0 {
                    return Some(false);
                }
                self.preferred = Some(height.saturating_add(1).saturating_sub(usize::from(y)));
                let updated = self.rows(height);
                self.preferred = Some(updated);
                Some(rows != updated)
            }
            _ => None,
        }
    }
}

fn dock_page(rows: usize, columns: usize) -> usize {
    rows.saturating_sub(if columns >= 44 && rows >= 4 { 3 } else { 2 })
}

#[allow(clippy::too_many_arguments)]
fn frame(
    report: &Value,
    nav: &navigation::Navigation,
    size: (usize, usize),
    read: &std::collections::BTreeMap<String, String>,
    colored: bool,
    split: Option<&mut DockSplit>,
    chat: Option<&mut ChatDock>,
    weekly: Option<(&mut ChatDock, WeeklyPhase)>,
) -> render::BoardFrame {
    frame_with_native_activity(report, nav, size, read, colored, split, chat, weekly, false)
}

#[allow(clippy::too_many_arguments)]
fn frame_with_native_activity(
    report: &Value,
    nav: &navigation::Navigation,
    size: (usize, usize),
    read: &std::collections::BTreeMap<String, String>,
    colored: bool,
    split: Option<&mut DockSplit>,
    chat: Option<&mut ChatDock>,
    weekly: Option<(&mut ChatDock, WeeklyPhase)>,
    native_activity_current: bool,
) -> render::BoardFrame {
    let (columns, height) = size;
    if let Some(menu) = &nav.main_switch {
        return render::main_switch_frame(menu, columns, height);
    }
    let chat = chat.filter(|dock| !private_chat_hidden(report, dock));
    let dock_rows = split.map(|split| {
        split.paint(
            height,
            weekly.is_none() && chat.is_some() && report["native_dock"].is_null(),
        )
    });
    if let Some((weekly, phase)) = weekly {
        let message = match phase {
            WeeklyPhase::Fresh => None,
            WeeklyPhase::Pending => Some(""),
            WeeklyPhase::Unavailable => Some("Weekly usage unavailable until the Board refreshes"),
        };
        let mut placeholder = message.map(|history| ChatDock {
            title: weekly.title.clone(),
            history: history.into(),
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        });
        let lines = if height >= 3 {
            let mut lines = dock_lines(
                columns,
                height,
                colored,
                placeholder.as_mut().unwrap_or(weekly),
                true,
                None,
            );
            let hint = match (phase, nav.notice.as_deref()) {
                (WeeklyPhase::Unavailable, _) => "u Retry Board · Esc Close",
                (_, Some(notice)) if notice.starts_with(QUOTA_CHECKING) => {
                    "Checking weekly quota · Esc Close"
                }
                (WeeklyPhase::Pending, _) => "Esc Close",
                (WeeklyPhase::Fresh, Some(QUOTA_UNAVAILABLE)) => {
                    "No idle Claude or AGY session · U Retry · Esc Close"
                }
                (WeeklyPhase::Fresh, _) => "↑↓ Scroll · U Refresh quota · Esc Close",
            };
            if let Some(footer) = lines.last_mut() {
                *footer = render::clip(hint, columns);
            }
            lines
        } else {
            let mut lines = vec![render::clip("Weekly usage · Esc Close", columns)];
            lines.resize(height, String::new());
            lines
        };
        return render::BoardFrame {
            lines,
            provider_rows: [None; 3],
            worker_rows: Default::default(),
            weekly_usage: None,
            main_switch: None,
        };
    }
    let (mut nav, board_height) = board_viewport(report, nav, height, dock_rows);
    let main = chat
        .as_ref()
        .and_then(|dock| dock.main.as_ref())
        .and_then(|id| render::pinned_main(report).filter(|main| call_identity(main) == *id));
    // A Main summary the reader dock cannot paint reads as the Main detail
    // card while the pinned Main, the first visible row, stays selected.
    let chat = match chat {
        Some(_) if dock_rows == Some(0) && nav.selected == 0 && main.is_some() => {
            nav.to_mut().detail = true;
            None
        }
        chat => chat,
    };
    let nav = nav.as_ref();
    let interactive = dock_rows.is_some();
    let mut board = render::board_frame_with_native_activity(
        report,
        nav,
        columns,
        board_height,
        read,
        colored,
        interactive,
        native_activity_current,
    );
    if interactive && let Some(provider) = report["native_dock"].as_str() {
        board.lines.resize(board_height, String::new());
        board.lines.push(render::native_dock_bar(
            provider,
            CLOSE_NATIVE,
            columns,
            colored,
        ));
    } else if board_height < height
        && let Some(chat) = chat
    {
        board.lines.resize(board_height, String::new());
        for hit in &mut board.provider_rows {
            if hit.is_some_and(|row| row >= board_height) {
                *hit = None;
            }
        }
        board.worker_rows.retain(|row, _| *row < board_height);
        board.weekly_usage = board.weekly_usage.filter(|hit| hit.row < board_height);
        board.lines.extend(dock_lines(
            columns,
            height - board_height,
            colored,
            chat,
            false,
            main.as_ref(),
        ));
    }
    board
}

/// Board rows and navigation (with any refresh-failure notice) that `frame`
/// paints, so key paging pages exactly the visible task rows.
fn board_viewport<'a>(
    report: &Value,
    nav: &'a navigation::Navigation,
    height: usize,
    dock_rows: Option<usize>,
) -> (std::borrow::Cow<'a, navigation::Navigation>, usize) {
    let message = if report["collection"]["pending"] == true {
        None
    } else {
        report["collection"]["error"]
            .as_str()
            .map(|reason| format!("Refresh failed: {reason}"))
    };
    let nav = match message {
        Some(message) => {
            let mut display = nav.clone();
            display.notice = Some(match &nav.notice {
                Some(notice) if notice != &message => format!("{message} · {notice}"),
                _ => message,
            });
            std::borrow::Cow::Owned(display)
        }
        None => std::borrow::Cow::Borrowed(nav),
    };
    let rows = match dock_rows {
        Some(_) if report["native_dock"].is_string() => height.saturating_sub(1),
        Some(dock) if dock >= 3 => height.saturating_sub(dock),
        _ => height,
    };
    (nav, rows)
}

fn key_page(
    report: &Value,
    nav: &navigation::Navigation,
    size: (usize, usize),
    dock_rows: Option<usize>,
    read: &std::collections::BTreeMap<String, String>,
) -> usize {
    let (nav, rows) = board_viewport(report, nav, size.1, dock_rows);
    render::page_capacity(report, &nav, size.0, rows, read)
}

fn dock_lines(
    columns: usize,
    dock: usize,
    colored: bool,
    chat: &mut ChatDock,
    weekly: bool,
    main: Option<&Value>,
) -> Vec<String> {
    let boxed = weekly && columns >= 44 && dock >= 4;
    let inner = if boxed { columns - 4 } else { columns };
    let page = if weekly {
        dock_page(dock, columns)
    } else {
        dock.saturating_sub(2)
    };
    // A Main summary is rebuilt for this paint's width and page, so no fact
    // wraps past the identity row that the default tail view must show.
    let history = match (&chat.main, main) {
        (Some(_), Some(main)) => render::main_bounded_lines(main, inner, page),
        (Some(_), None) => chat
            .history
            .lines()
            .map(|line| render::clip(line, inner))
            .take(page)
            .collect(),
        (None, _) => history_rows(&chat.history, inner.max(1), colored),
    };
    let latest = history.len().saturating_sub(page);
    chat.back = chat.back.min(latest);
    let title = if weekly {
        render::dock_top(&chat.title, columns, colored)
    } else {
        render::clip(&format!("↕ {}", chat.title), columns)
    };
    let mut lines = vec![title];
    let mut visible = history
        .into_iter()
        .skip(latest - chat.back)
        .take(page)
        .collect::<Vec<_>>();
    visible.resize(page, String::new());
    lines.extend(visible.into_iter().map(|line| {
        if boxed {
            render::dock_line(&line, columns)
        } else {
            line
        }
    }));
    if boxed {
        lines.push(render::dock_bottom(columns, colored));
    }
    lines.push(render::clip("↑↓ Scroll · Esc Close", columns));
    lines
}

fn history_rows(history: &str, columns: usize, colored: bool) -> Vec<String> {
    if history.trim().is_empty() {
        return vec![render::clip("No chat output yet", columns)];
    }
    let mut rows = Vec::new();
    let mut blank = true;
    for line in history.lines() {
        let visible = render::clean(line, 4096);
        let mut remaining = visible.trim_end().chars().count();
        if remaining == 0 {
            if !blank {
                rows.push(String::new());
                blank = true;
            }
            continue;
        }
        blank = false;
        let mut row = String::new();
        let mut styles = String::new();
        let mut width = 0;
        let mut start = 0;
        for code in crate::orchestrator::CHAT_SGR
            .find_iter(line)
            .map(Some)
            .chain([None])
        {
            let end = code.map_or(line.len(), |code| code.start());
            for ch in render::clean(&line[start..end], 4096)
                .chars()
                .take(remaining)
            {
                remaining -= 1;
                let ch = if render::cell_width(ch) > columns {
                    '…'
                } else {
                    ch
                };
                let cells = render::cell_width(ch);
                if width + cells > columns {
                    if !styles.is_empty() {
                        row.push_str("\x1b[0m");
                    }
                    rows.push(std::mem::replace(&mut row, styles.clone()));
                    width = 0;
                }
                row.push(ch);
                width += cells;
            }
            if remaining == 0 {
                break;
            }
            if let Some(code) = code {
                start = code.end();
                if colored {
                    row.push_str(code.as_str());
                    if matches!(code.as_str(), "\x1b[m" | "\x1b[0m") {
                        styles.clear();
                    } else {
                        styles.push_str(code.as_str());
                    }
                }
            }
        }
        if colored {
            row.push_str("\x1b[0m");
        }
        rows.push(row);
    }
    rows
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionFence {
    None,
    Changed,
    Native,
}

fn reconcile_selection_with_page(
    before: &[Value],
    after: &[Value],
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
    page: usize,
) {
    let selected_native = before.get(nav.selected).is_some_and(native_chat_child);
    if nav.reconcile_refresh(before, after, page) {
        if selected_native {
            *changed = SelectionFence::Native;
        } else if *changed == SelectionFence::None {
            *changed = SelectionFence::Changed;
        }
        nav.detail = false;
    }
}

#[cfg(test)]
fn reconcile_selection(
    before: &[Value],
    after: &[Value],
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
) {
    reconcile_selection_with_page(before, after, nav, changed, 1);
}

fn apply_selection_fence(
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
    key: &input::Key,
    count: usize,
) -> Option<navigation::Action> {
    const RETRY_DETAIL: &str = "selection changed; press D again for Call Detail";
    let direct_detail = *key == input::Key::Char('D') && !nav.editing;
    if *changed != SelectionFence::None
        && (direct_detail || matches!(key, input::Key::Enter | input::Key::Char('f')))
        && !nav.editing
    {
        nav.notice = Some(if direct_detail {
            RETRY_DETAIL.into()
        } else if *changed == SelectionFence::Native {
            "Transcript unavailable".into()
        } else {
            "selection changed; press Enter again to open".into()
        });
        *changed = SelectionFence::None;
        return Some(navigation::Action::None);
    }
    if direct_detail {
        nav.detail = count > 0;
        if nav.notice.as_deref() == Some(RETRY_DETAIL) {
            nav.notice = None;
        }
        return Some(navigation::Action::None);
    }
    if matches!(
        key,
        input::Key::Up
            | input::Key::Down
            | input::Key::PageUp
            | input::Key::PageDown
            | input::Key::Home
            | input::Key::End
            | input::Key::Scroll(_)
    ) {
        *changed = SelectionFence::None;
    }
    None
}

fn apply_board_key(
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
    key: &input::Key,
    report: &Value,
    before: &[Value],
    page: usize,
) -> navigation::Action {
    if let Some(action) = apply_selection_fence(nav, changed, key, before.len()) {
        return action;
    }
    nav.apply_with_calls(key, before, |n| render::visible(report, n), page)
}

#[cfg(test)]
fn apply_selection_key(
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
    key: &input::Key,
    count: usize,
    page: usize,
) -> navigation::Action {
    if let Some(action) = apply_selection_fence(nav, changed, key, count) {
        return action;
    }
    nav.apply(key, count, page)
}

#[cfg(test)]
fn preserve_selection(before: &[Value], after: &[Value], selected: usize) -> usize {
    before
        .get(selected)
        .map(call_identity)
        .and_then(|id| after.iter().position(|call| call_identity(call) == id))
        .unwrap_or_else(|| selected.min(after.len().saturating_sub(1)))
}

fn call_identity(call: &Value) -> (String, String) {
    (
        crate::text(call, "participant").to_owned(),
        crate::text(call, "attempt").to_owned(),
    )
}

fn format_chats_notice(room: &str, catalog: &Value) -> String {
    let mut joined = Vec::new();
    for p in crate::rows(&catalog["participants"]) {
        if p["joined"] == true {
            let name = p["alias"]
                .as_str()
                .filter(|v| !v.is_empty())
                .or_else(|| p["participant"].as_str())
                .unwrap_or("member");
            let role = crate::text(p, "role");
            let provider = crate::text(p, "provider");
            joined.push(format!("{name} ({role}/{provider})"));
        }
    }
    if joined.is_empty() {
        format!("Room {room}: no joined participants")
    } else {
        format!(
            "Room {room} ({} joined): {}",
            joined.len(),
            joined.join(", ")
        )
    }
}

fn draw(
    output: &mut dyn Write,
    frame: &[String],
    terminal: bool,
    height: usize,
    previous: &mut Vec<String>,
    force: bool,
) -> Result<()> {
    if terminal {
        if !force && previous == frame {
            return Ok(());
        }
        // One synchronized write; rows are overwritten, never blanked first.
        let full = force || previous.is_empty();
        let mut buffer = String::from("\x1b[?2026h");
        for (row, line) in frame.iter().enumerate() {
            if full || previous.get(row) != Some(line) {
                buffer.push_str(&format!("\x1b[{};1H\x1b[2K{line}", row + 1));
            }
        }
        if frame.len() < height && (full || previous.len() > frame.len()) {
            buffer.push_str(&format!("\x1b[{};1H\x1b[J", frame.len() + 1));
        }
        buffer.push_str("\x1b[?2026l");
        output.write_all(buffer.as_bytes()).map_err(Error::new)?;
    } else {
        for line in frame {
            writeln!(output, "{line}").map_err(Error::new)?;
        }
    }
    *previous = frame.to_vec();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DockSplit, draw, history_rows, provider_at_row};
    struct Counting(Vec<u8>, usize);
    impl std::io::Write for Counting {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.1 += 1;
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn rows(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    fn council_shortcut_selects_latest_attempt_in_current_room_despite_main_collision() {
        let mut report = serde_json::json!({
            "room":{"id":"room-one","participants":[{"participant":"main","role":"main","provider":"codex"}]},
            "room_attempts":{
                "old":{"tool":"panel-council","updated_at":"2026-01-01T00:00:00Z","refs":{"panel_room_id":"room-one","panel_room_participant":"main","panel_thread":"old"}},
                "new":{"tool":"panel-council","updated_at":"2026-01-02T00:00:00Z","refs":{"panel_room_id":"room-one","panel_room_participant":"main","panel_thread":"new"}},
                "other":{"tool":"panel-council","updated_at":"2026-01-03T00:00:00Z","refs":{"panel_room_id":"room-two","panel_room_participant":"main","panel_thread":"other"}},
                "conflict":{"tool":"panel-council","updated_at":"2026-01-04T00:00:00Z","refs":{"panel_room_id":"room-one","panel_room":"room-two","panel_thread":"conflict"}},
                "wrong":{"tool":"panel-main","updated_at":"2026-01-05T00:00:00Z","refs":{"panel_room_id":"room-one","panel_thread":"wrong"}}
            }
        });
        assert_eq!(
            super::latest_council(&report).unwrap()["refs"]["panel_thread"],
            "new"
        );
        report["room"]["id"] = serde_json::json!("room-two");
        assert_eq!(
            super::latest_council(&report).unwrap()["refs"]["panel_thread"],
            "other"
        );
        report["room"]["id"] = serde_json::json!("missing");
        assert!(super::latest_council(&report).is_none());
    }
    #[test]
    fn council_title_uses_lifecycle_label() {
        let call = serde_json::json!({"tool":"panel-council","refs":{"panel_label":"Council fixture title"}});
        assert_eq!(super::chat_title(&call), "Council fixture title");
    }
    #[test]
    fn council_box_rejects_a_log_with_a_different_thread_identity() {
        let log = br#"{"thread":"panel-council-example","role":"answer","agent":"claude","text":"One recorded position"}"#;
        assert_eq!(
            super::council_log(log, "panel-council-example").unwrap(),
            "claude · answer\nOne recorded position"
        );
        assert!(super::council_log(log, "panel-council-other").is_err());
    }
    #[test]
    fn provider_header_hit_and_history_layout() {
        let headers = [Some(4), None, Some(10)];
        assert_eq!(provider_at_row(&headers, 5), Some(0));
        assert_eq!(provider_at_row(&headers, 11), Some(2));
        assert_eq!(provider_at_row(&headers, 0), None);
        assert_eq!(provider_at_row(&headers, 6), None);
        assert_eq!(
            history_rows("한글 답변\n\x1b[31mOK\x1b[0m", 4, false),
            rows(&["한글", " 답", "변", "OK"])
        );
        assert_eq!(
            history_rows("Reply\n\n\n\x1b[0m\nStatus", 20, false),
            rows(&["Reply", "", "Status"])
        );
        let mut dock = super::ChatDock {
            title: "Claude · Recent chat".into(),
            history: "\x1b[31mPrevious answer\x1b[0m".into(),
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        };
        let report = serde_json::json!({
            "collection": {"ok": true},
            "room": {"task_id": "t1"},
            "plan": {"goal": "Current work", "tasks": {"t1": {"state": "running"}}},
            "provider_sessions": ["claude"],
            "provider_models": {"claude": "Sonnet 5.5"}
        });
        let frame = super::frame(
            &report,
            &Default::default(),
            (100, 36),
            &Default::default(),
            true,
            Some(&mut DockSplit::default()),
            Some(&mut dock),
            None,
        );
        let claude = frame.provider_rows[1].unwrap();
        assert!(frame.lines[claude].contains("Sonnet 5.5"));
        assert!(
            frame.lines[..24]
                .iter()
                .any(|line| line.contains("Current work"))
        );
        assert!(frame.lines[24].contains("Claude · Recent chat"));
        assert!(frame.lines[25].contains("\x1b[31mPrevious answer"));
        assert_eq!(frame.lines.len(), 36);
        let mut native_report = report;
        native_report["native_dock"] = serde_json::json!("claude");
        let frame = super::frame(
            &native_report,
            &Default::default(),
            (100, 36),
            &Default::default(),
            false,
            Some(&mut DockSplit::default()),
            Some(&mut dock),
            None,
        );
        assert_eq!(frame.lines.len(), 36);
        assert!(frame.lines[35].starts_with(super::CLOSE_NATIVE));
        assert!(frame.lines[35].contains("Claude Orchestrator"));
        assert!(
            !frame
                .lines
                .iter()
                .any(|line| line.contains("Previous answer"))
        );
        assert!(super::close_native_key(
            &crate::input::Key::Click(5, 36),
            (100, 36)
        ));
    }
    #[test]
    fn key_paging_uses_the_painted_board_viewport() {
        use crate::navigation::Navigation;
        let participants: Vec<_> = (0..60)
            .map(|n| {
                serde_json::json!({"participant":format!("P{n:02}"), "provider":"codex",
                    "role":"worker", "state":"working", "title":format!("Work {n}")})
            })
            .collect();
        let report = |dock: Option<&str>, error: Option<&str>| {
            serde_json::json!({"collection":{"ok":error.is_none(), "error":error},
                "native_dock":dock, "plan":{"goal":"Ship"},
                "room":{"id":"room", "participants":participants}})
        };
        let detail = Navigation {
            view: crate::layout::View::Detail,
            ..Default::default()
        };
        let noticed = Navigation {
            notice: Some("Saved".into()),
            ..Default::default()
        };
        let read = Default::default();
        let mut chat = super::ChatDock {
            title: "Worker result".into(),
            history: "Result".into(),
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        };
        for (report, nav, size, show_reader) in [
            (
                report(Some("claude"), None),
                Navigation::default(),
                (100, 36),
                false,
            ),
            (
                report(Some("claude"), None),
                detail.clone(),
                (100, 40),
                false,
            ),
            (
                report(Some("claude"), None),
                Navigation::default(),
                (100, 12),
                true,
            ),
            (report(None, None), detail, (100, 36), true),
            (report(None, None), Navigation::default(), (100, 12), false),
            (report(None, None), Navigation::default(), (100, 12), true),
            (report(None, None), Navigation::default(), (100, 10), true),
            (
                report(None, Some("boom")),
                Navigation::default(),
                (100, 36),
                false,
            ),
            (report(None, Some("boom")), noticed, (100, 30), false),
        ] {
            let mut split = DockSplit::default();
            let painted = super::frame(
                &report,
                &nav,
                size,
                &read,
                false,
                Some(&mut split),
                show_reader.then_some(&mut chat),
                None,
            );
            let dock = Some(split.painted_rows);
            let page = super::key_page(&report, &nav, size, dock, &read);
            assert_eq!(painted.lines.len(), size.1, "{size:?} {dock:?}");
            assert!(page < 60, "{size:?} {dock:?}");
            assert_eq!(page, painted.worker_rows.len(), "{size:?} {dock:?} {nav:?}");
        }
    }

    #[test]
    fn weekly_usage_click_and_key_require_the_current_board_paint() {
        use super::{
            CLOSE_NATIVE, ChatDock, QUOTA_CHECKING, QUOTA_UNAVAILABLE, RefreshPhase, WeeklyKey,
            WeeklyPaint, WeeklyPhase, WeeklyUsage, frame,
        };
        use crate::{input::Key, render};
        use oms_panel_core::Value;
        use serde_json::json;

        let report = json!({
            "collection": {"ok":true}, "native_dock":"claude",
            "provider_status":{"codex":{"source":"session log","weekly_used":25}}
        });
        let size = (80, 13);
        let paint = |report: &Value, detail: Option<(&mut ChatDock, WeeklyPhase)>, notice| {
            let nav = crate::navigation::Navigation {
                notice,
                ..Default::default()
            };
            frame(
                report,
                &nav,
                size,
                &Default::default(),
                true,
                Some(&mut DockSplit::default()),
                None,
                detail,
            )
        };
        let shown = paint(&report, None, None);
        let hit = shown.weekly_usage.unwrap();
        let target = || WeeklyPaint {
            hit: Some(hit),
            size,
        };
        let y = u16::try_from(hit.row + 1).unwrap();
        let right = u16::try_from(hit.columns).unwrap();
        let mut weekly = WeeklyUsage::default();
        let pending = RefreshPhase::Pending.report(report.clone());
        let failed = RefreshPhase::Failed("fixture error".into()).report(report.clone());

        for (data, snapshot_ok, awaiting, expected, message) in [
            (&pending, false, false, WeeklyPhase::Pending, "Esc Close"),
            (&report, true, true, WeeklyPhase::Pending, "Esc Close"),
            (
                &failed,
                false,
                false,
                WeeklyPhase::Unavailable,
                "Weekly usage unavailable",
            ),
        ] {
            let phase = WeeklyPhase::from_report(data, snapshot_ok).awaiting(awaiting);
            assert_eq!(phase, expected);
            let hit = paint(data, None, None).weekly_usage.unwrap();
            let key_at = |weekly: &mut WeeklyUsage, key, hit| {
                weekly.key(
                    &key,
                    data,
                    WeeklyPaint { hit, size },
                    size,
                    Some(phase),
                    false,
                )
            };
            let click = Key::Click(
                u16::try_from(hit.columns).unwrap(),
                u16::try_from(hit.row + 1).unwrap(),
            );
            for key in [Key::Char('w'), click] {
                let mut weekly = WeeklyUsage::default();
                assert_eq!(
                    key_at(&mut weekly, key.clone(), Some(hit)),
                    WeeklyKey::Handled
                );
                assert!(
                    weekly
                        .detail
                        .as_ref()
                        .is_some_and(|detail| detail.history.is_empty()),
                    "same-scope {phase:?} opens an empty overlay for {key:?}"
                );
                for key in [Key::Char('U'), Key::Char('u')] {
                    assert_eq!(key_at(&mut weekly, key, Some(hit)), WeeklyKey::Handled);
                }

                let display = paint(data, weekly.overlay(phase), None);
                let text = render::clean(&display.lines.join("\n"), 4096);
                assert!(text.contains(message));
                assert!(!text.contains("Refreshing Board"));
                for carried in ["75%", "session log", "fixture error"] {
                    assert!(!text.contains(carried), "{text}");
                }
                assert_eq!(display.lines.len(), size.1);
                assert!(display.weekly_usage.is_none());
                weekly.repaint = false;
                assert_eq!(
                    key_at(&mut weekly, Key::Char('U'), display.weekly_usage),
                    WeeklyKey::Handled
                );
                assert!(!weekly.repaint, "held U does not dispatch or queue");

                if phase == WeeklyPhase::Unavailable {
                    assert_eq!(
                        display.lines.last().unwrap().trim_end(),
                        "u Retry Board · Esc Close"
                    );
                    for (actual, scope_phase, editing) in [
                        ((size.0 + 1, size.1), Some(phase), false),
                        (size, None, false),
                        (size, Some(phase), true),
                    ] {
                        assert_eq!(
                            weekly.key(
                                &Key::Char('u'),
                                data,
                                WeeklyPaint { hit: None, size },
                                actual,
                                scope_phase,
                                editing,
                            ),
                            WeeklyKey::Handled
                        );
                        assert!(!weekly.repaint);
                    }
                    assert_eq!(
                        key_at(&mut weekly, Key::Char('u'), display.weekly_usage),
                        WeeklyKey::RefreshBoard
                    );
                    assert_eq!(
                        key_at(&mut weekly, Key::Char('u'), display.weekly_usage),
                        WeeklyKey::Handled
                    );
                    assert_eq!(
                        key_at(&mut weekly, Key::Char('U'), display.weekly_usage),
                        WeeklyKey::Handled
                    );
                    let retry = paint(data, weekly.overlay(phase), None);
                    weekly.repaint = false;
                    assert_eq!(
                        key_at(&mut weekly, Key::Char('U'), retry.weekly_usage),
                        WeeklyKey::Handled
                    );
                    let waiting = paint(&pending, weekly.overlay(WeeklyPhase::Pending), None);
                    assert!(
                        waiting
                            .lines
                            .iter()
                            .all(|line| !line.contains("Refreshing Board"))
                    );
                } else {
                    assert_eq!(
                        key_at(&mut weekly, Key::Char('u'), display.weekly_usage),
                        WeeklyKey::Handled
                    );
                    assert!(!weekly.repaint);
                }
                assert_eq!(
                    key_at(&mut weekly, Key::Escape, display.weekly_usage),
                    WeeklyKey::Handled
                );
                assert!(weekly.detail.is_none() && weekly.repaint);
            }
        }
        let fresh = Some(WeeklyPhase::Fresh);
        let held = Some(WeeklyPhase::Pending);
        for (data, actual, phase) in [
            (
                &failed,
                (size.0 + 1, size.1),
                Some(WeeklyPhase::Unavailable),
            ),
            (&failed, size, None),
            (&report, (size.0 + 1, size.1), fresh),
            (&pending, (size.0, size.1 - 1), held),
            (&pending, size, None),
            (&report, size, None),
        ] {
            for key in [Key::Click(right, y), Key::Char('w')] {
                assert_eq!(
                    weekly.key(&key, data, target(), actual, phase, false),
                    WeeklyKey::Handled
                );
                assert!(weekly.detail.is_none() && !weekly.repaint);
            }
        }
        for key in [
            Key::Click(right + 1, y),
            Key::Click(0, y),
            Key::Click(right, y + 1),
        ] {
            assert_eq!(
                weekly.key(&key, &report, target(), size, fresh, false),
                WeeklyKey::Pass
            );
        }
        assert_eq!(
            weekly.key(&Key::Char('w'), &report, target(), size, fresh, true),
            WeeklyKey::Pass
        );
        assert_eq!(
            weekly.key(&Key::Click(right, y), &report, target(), size, fresh, false),
            WeeklyKey::Handled
        );
        let detail = paint(
            &report,
            weekly.detail.as_mut().map(|d| (d, WeeklyPhase::Fresh)),
            None,
        );
        let plain = render::clean(&detail.lines.join(" "), 4_096);
        assert!(plain.contains("75% left"));
        assert!(plain.contains("Codex session log"));
        assert!(detail.weekly_usage.is_none());
        assert!(detail.provider_rows.iter().all(Option::is_none) && detail.worker_rows.is_empty());
        assert_eq!(detail.lines.len(), size.1);
        assert!(detail.lines.last().unwrap().contains("U Refresh quota"));
        assert!(shown.lines[size.1 - 1].contains(CLOSE_NATIVE));

        weekly.detail.as_mut().unwrap().back = 2;
        for (retained, phase, notice) in [
            (&pending, WeeklyPhase::Pending, "Esc Close"),
            (
                &failed,
                WeeklyPhase::Unavailable,
                "Weekly usage unavailable",
            ),
        ] {
            let display = paint(retained, weekly.detail.as_mut().map(|d| (d, phase)), None);
            assert!(display.lines.iter().any(|line| line.contains(notice)));
            assert!(display.lines.iter().all(|line| !line.contains("75% left")));
            assert!(display.lines.last().unwrap().contains("Esc Close"));
            assert_eq!(weekly.detail.as_ref().unwrap().back, 2);
        }
        for (phase, notice, feedback) in [
            (
                WeeklyPhase::Pending,
                QUOTA_CHECKING,
                "Checking weekly quota",
            ),
            (
                WeeklyPhase::Fresh,
                QUOTA_UNAVAILABLE,
                "No idle Claude or AGY session",
            ),
        ] {
            let display = paint(
                &report,
                weekly.detail.as_mut().map(|d| (d, phase)),
                Some(notice.into()),
            );
            assert_eq!(display.lines.len(), 13);
            assert!(display.lines.last().unwrap().contains(feedback));
        }

        weekly.repaint = false;
        for (actual, phase, editing) in [
            (size, None, false),
            (size, Some(WeeklyPhase::Unavailable), false),
            (size, held, false),
            ((size.0, size.1 - 1), fresh, false),
            (size, fresh, true),
        ] {
            assert_eq!(
                weekly.key(&Key::Char('U'), &report, target(), actual, phase, editing),
                WeeklyKey::Handled
            );
            assert!(!weekly.repaint);
        }
        assert_eq!(
            weekly.key(&Key::Char('U'), &report, target(), size, fresh, false),
            WeeklyKey::RefreshQuota
        );
        let close_native = Key::Click(5, u16::try_from(size.1).unwrap());
        for (key, result) in [
            (Key::Char('U'), WeeklyKey::Handled),
            (close_native.clone(), WeeklyKey::Handled),
            (Key::Eof, WeeklyKey::Pass),
            (Key::Escape, WeeklyKey::Handled),
            (close_native, WeeklyKey::Handled),
            (Key::Eof, WeeklyKey::Pass),
        ] {
            assert_eq!(
                weekly.key(&key, &report, target(), size, fresh, false),
                result
            );
        }
        assert!(weekly.detail.is_none());
        assert_eq!(report["native_dock"], "claude");

        weekly.repaint = false;
        assert_eq!(
            weekly.key(
                &Key::Char('w'),
                &report,
                WeeklyPaint { hit: None, size },
                size,
                fresh,
                false
            ),
            WeeklyKey::Handled
        );
        assert!(weekly.detail.is_some());
    }

    #[test]
    fn weekly_usage_and_call_clicks_use_the_full_idle_board() {
        use super::{DockSplit, WeeklyKey, WeeklyPaint, WeeklyPhase, WeeklyUsage, frame};
        use crate::{input::Key, render};
        use serde_json::json;

        let now = oms_panel_core::timestamp();
        let report = json!({
            "collection":{"ok":true},
            "board_checkout":{"attempt":"main-attempt","branch":"oms/review","worktree":"proof-tree"},
            "provider_sessions":["codex","claude","antigravity"],
            "provider_activity":{"codex":"working","claude":{"state":"idle"}},
            "provider_status":{
                "codex":{"source":"session log","weekly_used":25},
                "claude":{"source":"HUD cache","weekly_used":0},
                "antigravity":{"weekly_native_observed_at":now,"weekly_used":5.38}
            },
            "room":{"task_id":"t1","participants":[
                {"participant":"main","attempt":"main-attempt","role":"main","state":"working","joined":true},
                {"participant":"call-worker","role":"worker","state":"working","joined":true,
                 "title":"Review terminal layout"}
            ]},
            "plan":{"goal":"Keep Board usable","tasks":{"t1":{"state":"running"}}}
        });
        let nav = crate::navigation::Navigation {
            filter: "Review".into(),
            notice: Some(format!("First {}Second notice", "a".repeat(72))),
            ..Default::default()
        };
        let spy = crate::orchestrator::codex_chat_fixture::BoardSpy::new("main-dock");
        for height in [12, 13] {
            let size = (80, height);
            let board = frame(
                &report,
                &nav,
                size,
                &Default::default(),
                true,
                Some(&mut DockSplit::default()),
                None,
                None,
            );
            assert_eq!(board.lines.len(), height);
            assert!(
                board
                    .lines
                    .iter()
                    .all(|line| !line.contains("Select an orchestrator"))
            );
            assert!(board.lines[height - 1].contains("Enter"));
            for text in ["Filter /Review", "First", "Goal: Keep Board usable"] {
                assert!(
                    board.lines.iter().any(|line| line.contains(text)),
                    "{height} rows, missing {text}: {:?}",
                    board.lines
                );
            }
            let (&main_row, main_hit) = board.worker_rows.first_key_value().unwrap();
            let (&row, worker_hit) = board
                .worker_rows
                .iter()
                .find(|(_, (participant, _))| participant == "call-worker")
                .unwrap();
            assert_eq!(main_hit.0, "main");
            assert!(main_row < row && board.lines[main_row].contains("▸"));
            assert!(row < height - 1 && board.lines[row].contains("Review terminal layout"));

            let calls = render::visible(&report, &nav);
            let mut focus = nav.clone();
            let mut fence = super::SelectionFence::None;
            super::apply_selection_key(&mut focus, &mut fence, &Key::Down, calls.len(), 1);
            assert_eq!(super::call_identity(&calls[focus.selected]), *worker_hit);
            super::apply_selection_key(&mut focus, &mut fence, &Key::Up, calls.len(), 1);
            assert_eq!(super::call_identity(&calls[focus.selected]), *main_hit);
            assert_eq!(
                super::apply_selection_key(&mut focus, &mut fence, &Key::Enter, calls.len(), 1),
                crate::navigation::Action::Open
            );
            assert!(focus.detail);
            let picked = super::select_painted_call(
                &report, &calls, worker_hit, &mut focus, &mut fence, true,
            );
            assert_eq!(picked.map(super::call_identity), Some(worker_hit.clone()));
            assert!(!focus.detail);
            fence = super::SelectionFence::Native;
            assert!(
                super::select_painted_call(
                    &report, &calls, main_hit, &mut focus, &mut fence, false
                )
                .is_none()
            );
            assert!(!focus.detail);
            let main =
                super::select_painted_call(&report, &calls, main_hit, &mut focus, &mut fence, true)
                    .expect("an authorized Main click opens the reader dock");
            assert!(!focus.detail && fence == super::SelectionFence::None);
            assert_eq!(super::call_identity(&calls[focus.selected]), *main_hit);
            let stop = std::sync::atomic::AtomicBool::new(false);
            let worker = &calls[1];
            let dock = |report: &serde_json::Value, call: &serde_json::Value| {
                super::row_dock(std::path::Path::new("."), report, call, &spy, &stop).unwrap()
            };
            let worker_view =
                super::worker_dock(std::path::Path::new("."), worker, &spy, &stop).unwrap();
            let row_view = dock(&report, worker);
            assert_eq!(
                (&row_view.title, &row_view.history),
                (&worker_view.title, &worker_view.history)
            );
            assert_eq!(
                dock(&report, main).history,
                "Provider unavailable · Model unavailable · working · Tokens unavailable\n\
                 Work unavailable · Task unavailable"
            );
            let mut summary = report.clone();
            for (key, value) in [
                ("provider", "claude"),
                ("model", "opus"),
                ("title", "Own goal"),
            ] {
                summary["room"]["participants"][0][key] = json!(value);
            }
            let summary_calls = render::visible(&summary, &focus);
            for rows in [13, 24] {
                let docked = frame(
                    &summary,
                    &focus,
                    (80, rows),
                    &Default::default(),
                    false,
                    Some(&mut DockSplit::default()),
                    Some(&mut dock(&summary, &summary_calls[0])),
                    None,
                );
                let at = docked
                    .lines
                    .iter()
                    .position(|line| line.starts_with("↕ Overall Main"))
                    .expect("Main summary in the reader dock");
                assert_eq!(
                    docked.lines[at + 1..at + 3]
                        .iter()
                        .map(|line| line.trim_end())
                        .collect::<Vec<_>>(),
                    [
                        "Claude · opus · working · Tokens unavailable",
                        "Work Own goal · Task unavailable"
                    ],
                    "{:?}",
                    docked.lines
                );
                assert!(
                    docked
                        .lines
                        .iter()
                        .all(|line| !line.contains("Call Detail"))
                );
                assert!(
                    rows < 24
                        || docked.worker_rows.iter().any(|(row, (id, _))| {
                            id == "call-worker"
                                && *row < at
                                && docked.lines[*row].contains("Review terminal")
                        }),
                    "{:?}",
                    docked.lines
                );
            }
            // Under 11 rows the reader dock gets none; an open Main summary
            // reads as the Main card and the selection stays on Main.
            let short_split = DockSplit::default();
            assert_eq!((short_split.rows(10), short_split.rows(11)), (0, 4));
            let shown = |rows: usize, selected: usize, call: Option<&serde_json::Value>| {
                let mut split = DockSplit::default();
                let lines = frame(
                    &summary,
                    &crate::navigation::Navigation {
                        selected,
                        ..focus.clone()
                    },
                    (80, rows),
                    &Default::default(),
                    false,
                    Some(&mut split),
                    call.map(|call| dock(&summary, call)).as_mut(),
                    None,
                )
                .lines;
                assert_eq!(split.painted_rows, 0);
                lines
            };
            for rows in [8, 10] {
                let card = shown(rows, focus.selected, Some(&summary_calls[0]));
                let at = card
                    .iter()
                    .position(|line| line.contains("Overall Main") && line.contains("Esc back"))
                    .unwrap_or_else(|| panic!("{rows} rows: {card:?}"));
                assert_eq!(
                    card[at + 1..at + 3]
                        .iter()
                        .map(|line| line.trim_matches(['│', ' ']))
                        .collect::<Vec<_>>(),
                    [
                        "Claude · opus · working · Tokens unavailable",
                        "Work Own goal · Task unavailable"
                    ],
                    "{card:?}"
                );
                assert!(
                    card.iter()
                        .all(|line| !line.starts_with('↕') && !line.contains("Call Detail"))
                );
                assert_eq!(
                    super::call_identity(&summary_calls[focus.selected]),
                    *main_hit
                );
                assert!(!focus.detail && fence == super::SelectionFence::None);
                let mut refused = (focus.clone(), super::SelectionFence::Changed);
                for open in [false, true] {
                    let action = super::apply_selection_key(
                        &mut refused.0,
                        &mut refused.1,
                        &Key::Enter,
                        summary_calls.len(),
                        1,
                    );
                    assert_eq!(action == crate::navigation::Action::Open, open);
                }
                assert_eq!(
                    super::call_identity(&summary_calls[refused.0.selected]),
                    *main_hit
                );
                // Escape closes the dock to this Board; a worker dock that
                // cannot paint leaves it unchanged, as does moving off Main.
                let board = shown(rows, focus.selected, None);
                assert!(board.iter().all(|line| !line.contains("Esc back")));
                assert_eq!(shown(rows, focus.selected, Some(worker)), board);
                assert_eq!(
                    shown(rows, 1, Some(&summary_calls[0])),
                    shown(rows, 1, None)
                );
            }
            // A long Work title clips instead of wrapping: the short card and
            // the reader dock keep identity, a labeled Work and Task in frame.
            let mut long = summary.clone();
            long["room"]["participants"][0]["title"] = json!("Overall Main goal ".repeat(12));
            long["room"]["participants"][0]["task_id"] = json!("t-main-42");
            let long_calls = render::visible(&long, &focus);
            let labeled_work = |facts: &str| {
                facts.split(['\n', '·']).any(|piece| {
                    let piece = piece.trim_matches(['│', ' ']);
                    piece.starts_with("Work Ov") && piece.ends_with('…')
                })
            };
            for rows in [8, 9, 10, 11, 13, 24] {
                for columns in [40, 50, 60, 80] {
                    let mut split = DockSplit::default();
                    let card = frame(
                        &long,
                        &focus,
                        (columns, rows),
                        &Default::default(),
                        false,
                        Some(&mut split),
                        Some(&mut dock(&long, &long_calls[0])),
                        None,
                    )
                    .lines;
                    let at = card
                        .iter()
                        .position(|line| {
                            line.starts_with("↕ Overall Main")
                                || line.contains("Overall Main") && line.contains("Esc back")
                        })
                        .unwrap_or_else(|| panic!("{columns}x{rows}: {card:?}"));
                    assert_eq!(rows > 10, split.painted_rows > 0, "{columns}x{rows}");
                    let end = card.len() - 1;
                    let facts = card[at + 1..end].join("\n");
                    let identity = if rows > 10 { &card[at + 1] } else { &facts };
                    for fact in ["Claude", "opus", "working"] {
                        assert!(identity.contains(fact), "{columns}x{rows} {fact}: {card:?}");
                    }
                    for fact in ["Tokens unavailable", "Task t-main-42"] {
                        assert!(facts.contains(fact), "{columns}x{rows} {fact}: {card:?}");
                    }
                    assert!(labeled_work(&facts), "{columns}x{rows}: {card:?}");
                    assert!(card.len() <= rows, "{columns}x{rows}: {card:?}");
                    assert!(
                        card.iter().all(|line| render::width(line) <= columns),
                        "{columns}x{rows}: {card:?}"
                    );
                }
            }
            // The dock rebuilds the summary at each painted width, so a
            // narrow repaint shows identity and restores the wide frame.
            let mut open = dock(&long, &long_calls[0]);
            let resized = [80, 40, 80].map(|columns| {
                let lines = frame(
                    &long,
                    &focus,
                    (columns, 13),
                    &Default::default(),
                    false,
                    Some(&mut DockSplit::default()),
                    Some(&mut open),
                    None,
                )
                .lines;
                let at = lines
                    .iter()
                    .position(|line| line.starts_with("↕ Overall Main"))
                    .unwrap();
                assert!(lines[at + 1].starts_with("Claude · opus · working"));
                assert!(labeled_work(&lines[at + 1..].join("\n")), "{lines:?}");
                assert!(lines.len() <= 13 && open.back == 0);
                assert!(lines.iter().all(|line| render::width(line) <= columns));
                lines
            });
            assert_eq!(resized[0], resized[2]);
            assert_ne!(resized[0], resized[1]);
            // Worker docks still wrap their history rather than clip it.
            let mut answer = worker.clone();
            answer["native_answer"] = json!("Worker answer ".repeat(8));
            let mut worker_open =
                super::worker_dock(std::path::Path::new("."), &answer, &spy, &stop).unwrap();
            let painted = frame(
                &report,
                &focus,
                (40, 13),
                &Default::default(),
                false,
                Some(&mut DockSplit::default()),
                Some(&mut worker_open),
                None,
            )
            .lines;
            let wrapped = super::history_rows(&worker_open.history, 40, false);
            assert!(wrapped.len() > 2);
            assert_eq!(
                painted[painted.len() - 3..painted.len() - 1],
                wrapped[wrapped.len() - 2..]
            );
            // A refresh, pending or not, rereads an open Main summary; a
            // replaced or vanished Main closes it. Worker docks stay as they are.
            let pending = |report: &serde_json::Value| {
                let mut pending = report.clone();
                pending["collection"]["pending"] = json!(true);
                pending
            };
            for next in [summary.clone(), pending(&summary)] {
                let mut open = Some(dock(&report, main));
                super::settle_main_dock(&mut open, &next);
                assert_eq!(
                    open.map(|dock| dock.history),
                    Some(
                        "Claude · opus · working · Tokens unavailable\n\
                         Work Own goal · Task unavailable"
                            .to_owned()
                    )
                );
            }
            let mut replaced = summary.clone();
            replaced["room"]["participants"][0]["participant"] = json!("next-main");
            let mut vanished = summary.clone();
            vanished["room"]["participants"][0]["role"] = json!("worker");
            for next in [&replaced, &pending(&replaced), &vanished] {
                let mut stale = Some(dock(&summary, &summary_calls[0]));
                super::settle_main_dock(&mut stale, next);
                assert!(stale.is_none());
                let mut kept = Some(dock(&summary, worker));
                super::settle_main_dock(&mut kept, next);
                assert_eq!(
                    kept.map(|dock| (dock.title, dock.history)),
                    Some((worker_view.title.clone(), worker_view.history.clone()))
                );
            }
            let unopened = frame(
                &summary,
                &focus,
                (80, 24),
                &Default::default(),
                false,
                Some(&mut DockSplit::default()),
                None,
                None,
            );
            assert!(unopened.lines.iter().all(|line| !line.starts_with('↕')));
            // D over an open Main dock leaves the card as the one surface, so
            // one Escape (the loop's dock-first close needs a dock) returns to
            // the plain Board on Main; Enter on the card swaps it back for the
            // dock. A refused D keeps the dock; worker docks stay.
            let paint = |nav: &crate::navigation::Navigation,
                         chat: &mut Option<super::ChatDock>| {
                let mut split = DockSplit::default();
                let lines = frame(
                    &summary,
                    nav,
                    (80, 24),
                    &Default::default(),
                    false,
                    Some(&mut split),
                    chat.as_mut(),
                    None,
                )
                .lines;
                let cards = lines
                    .iter()
                    .filter(|line| line.contains("Esc back"))
                    .count();
                let docks = lines.iter().filter(|line| line.starts_with('↕')).count();
                assert!(cards + docks <= 1, "{lines:?}");
                lines
            };
            let detail_key = |nav: &mut crate::navigation::Navigation,
                              fence: &mut super::SelectionFence,
                              chat: &mut Option<super::ChatDock>| {
                super::apply_selection_key(nav, fence, &Key::Char('D'), summary_calls.len(), 1);
                super::card_replaces_main_dock(nav, chat);
            };
            let main_dock = || Some(dock(&summary, &summary_calls[0]));
            let mut refused = (focus.clone(), super::SelectionFence::Changed, main_dock());
            detail_key(&mut refused.0, &mut refused.1, &mut refused.2);
            assert!(!refused.0.detail && refused.2.is_some_and(|dock| dock.main.is_some()));
            let (mut card, mut fence, mut chat) =
                (focus.clone(), super::SelectionFence::None, main_dock());
            assert!(
                paint(&card, &mut chat)
                    .iter()
                    .any(|line| line.starts_with("↕ Overall Main"))
            );
            detail_key(&mut card, &mut fence, &mut chat);
            assert!(card.detail && chat.is_none());
            assert!(
                paint(&card, &mut chat)
                    .iter()
                    .any(|line| line.contains("Esc back"))
            );
            super::apply_selection_key(&mut card, &mut fence, &Key::Escape, summary_calls.len(), 1);
            assert_eq!(paint(&card, &mut chat), unopened.lines);
            assert_eq!(
                super::call_identity(&summary_calls[card.selected]),
                *main_hit
            );
            detail_key(&mut card, &mut fence, &mut chat);
            assert_eq!(
                super::apply_selection_key(
                    &mut card,
                    &mut fence,
                    &Key::Enter,
                    summary_calls.len(),
                    1
                ),
                crate::navigation::Action::Open
            );
            let at = std::path::Path::new(".");
            assert!(super::open_row_dock(
                at,
                &summary,
                &summary_calls[0],
                &mut card,
                &mut chat,
                &spy,
                &stop
            ));
            assert!(!card.detail && chat.as_ref().is_some_and(|dock| dock.main.is_some()));
            assert!(
                paint(&card, &mut chat)
                    .iter()
                    .any(|line| line.starts_with("↕ Overall Main"))
            );
            let mut kept = (
                focus.clone(),
                super::SelectionFence::None,
                Some(dock(&summary, worker)),
            );
            kept.0.selected = 1;
            detail_key(&mut kept.0, &mut kept.1, &mut kept.2);
            assert!(kept.0.detail);
            assert_eq!(
                kept.2.map(|dock| dock.title),
                Some(worker_view.title.clone())
            );
            // A Main dock a short Board hides behind a worker card closes too.
            let mut hidden = (kept.0.clone(), super::SelectionFence::None, main_dock());
            hidden.0.detail = false;
            detail_key(&mut hidden.0, &mut hidden.1, &mut hidden.2);
            assert!(hidden.0.detail && hidden.2.is_none());

            let hit = board.weekly_usage.expect("Weekly fits on the full Board");
            assert!(
                board
                    .lines
                    .iter()
                    .any(|line| line.contains("Second notice"))
            );
            assert_eq!(hit.row, 1);
            assert!(board.lines[hit.row].contains('\x1b'));
            let weekly_text = render::clean(&board.lines[hit.row], 4096);
            assert_eq!(
                weekly_text.trim_end(),
                "Weekly left · Codex 75%* │ Claude 100%- │ AGY Gemini 95%?"
            );
            assert_eq!(hit.columns, render::width(weekly_text.trim_end()));
            let (x, y) = (
                u16::try_from(hit.columns).unwrap(),
                u16::try_from(hit.row + 1).unwrap(),
            );
            let paint = || WeeklyPaint {
                hit: Some(hit),
                size,
            };
            let mut weekly = WeeklyUsage::default();
            let fresh = Some(WeeklyPhase::Fresh);
            assert_eq!(
                weekly.key(&Key::Click(x + 1, y), &report, paint(), size, fresh, false),
                WeeklyKey::Pass
            );
            for key in [Key::Char('w'), Key::Click(x, y)] {
                let mut weekly = WeeklyUsage::default();
                assert_eq!(
                    weekly.key(&key, &report, paint(), size, fresh, false),
                    WeeklyKey::Handled
                );
                let detail = frame(
                    &report,
                    &nav,
                    size,
                    &Default::default(),
                    true,
                    Some(&mut DockSplit::default()),
                    None,
                    weekly
                        .detail
                        .as_mut()
                        .map(|detail| (detail, WeeklyPhase::Fresh)),
                );
                let text = render::clean(&detail.lines.join("\n"), 4096);
                for evidence in ["75% left", "100% left", "95% left", "AGY Gemini quota"] {
                    assert!(text.contains(evidence), "{text}");
                }
                assert_eq!(
                    text.matches("Activity · * working · - idle · ? unknown · ~ pending · ! stale")
                        .count(),
                    1
                );
                assert_eq!(detail.lines.len(), height);
                assert!(detail.weekly_usage.is_none());
            }
        }

        // 80x13 scroll + unlabelled current 23% left + registered Codex accounts + Pending/scope retention.
        use super::{BoardScope, InteractiveRefresh, RefreshKey, RefreshPhase, reset_weekly_scope};
        use crate::board_refresh::BoardRefresh;
        use crate::orchestrator::codex_chat_fixture::BoardSpy;
        use oms_panel_core::{
            CommandOutput, Error, NativeCommand, PeerExecutor, Result, Services, Value,
            state::Snapshot,
        };
        use std::path::Path;
        use std::sync::{atomic::AtomicBool, mpsc};
        use std::time::{Duration, Instant};

        struct ScopeCollector {
            answer: mpsc::Receiver<Result<Value>>,
        }
        impl PeerExecutor for ScopeCollector {
            fn execute(
                &self,
                _: &oms_panel_core::services::ExecutionRequest,
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                panic!("collector must not execute processes")
            }
        }
        impl Services for ScopeCollector {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[std::ffi::OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                let value = match command {
                    NativeCommand::Events => json!([]),
                    NativeCommand::Room => self
                        .answer
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(Error::new)??,
                    _ => panic!("unexpected collector command {command:?}"),
                };
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&value).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }

        let size = (80, 13);
        let now_f = now as f64;
        let accounts_report = json!({
            "collection":{"ok":true},
            "provider_sessions":["codex","claude","antigravity"],
            "provider_activity":{"codex":"working","claude":{"state":"idle"}},
            "provider_accounts":{"codex":{"account":"primary","pinned":false,"verification":"verified"}},
            "provider_status":{
                "codex":{
                    "source":"session log",
                    "account_label":"primary",
                    "weekly_used":77,
                    "observed_at":now_f - 45.0,
                    "weekly_resets_at":now_f + 7230.0,
                    "accounts":{
                        "alternate":{"source":"session log","weekly_used":12,"observed_at":now_f - 45.0,"resets_at":now_f + 7230.0},
                        "primary":{"source":"session log","weekly_used":77,"observed_at":now_f - 45.0,"resets_at":now_f + 7230.0},
                        "old-acct":{"source":"session log","weekly_used":15,"observed_at":now_f - 605.0,"resets_at":now_f + 7230.0},
                        "expired-acct":{"source":"session log","weekly_used":40,"observed_at":now_f - 125.0,"resets_at":now_f - 5.0}
                    }
                },
                "claude":{"source":"HUD cache","weekly_used":0,"observed_at":now_f - 45.0,"weekly_resets_at":now_f + 7230.0},
                "antigravity":{"source":"unavailable"}
            },
            "room":{"id":"room-a","closed":false,"participants":[]}
        });
        let accounts_board = frame(
            &accounts_report,
            &nav,
            size,
            &Default::default(),
            true,
            Some(&mut DockSplit::default()),
            None,
            None,
        );
        let accounts_hit = accounts_board
            .weekly_usage
            .expect("Weekly fits at 80x13 with Codex accounts");
        let strip_text = render::clean(&accounts_board.lines[accounts_hit.row], 4096);
        assert!(strip_text.contains("Codex 23%"), "{strip_text}");
        assert!(
            !strip_text.contains("primary")
                && !strip_text.contains("alternate")
                && !strip_text.contains('@'),
            "{strip_text}"
        );
        let accounts_paint = || WeeklyPaint {
            hit: Some(accounts_hit),
            size,
        };
        let (ax, ay) = (
            u16::try_from(accounts_hit.columns).unwrap(),
            u16::try_from(accounts_hit.row + 1).unwrap(),
        );
        let fresh = Some(WeeklyPhase::Fresh);
        for open_key in [Key::Click(ax, ay), Key::Char('w')] {
            let mut weekly = WeeklyUsage::default();
            assert_eq!(
                weekly.key(
                    &open_key,
                    &accounts_report,
                    accounts_paint(),
                    size,
                    fresh,
                    false
                ),
                WeeklyKey::Handled
            );
            let top = frame(
                &accounts_report,
                &nav,
                size,
                &Default::default(),
                true,
                Some(&mut DockSplit::default()),
                None,
                weekly.overlay(WeeklyPhase::Fresh),
            );
            let top_lines: Vec<String> = top.lines.iter().map(|l| render::clean(l, 4096)).collect();
            let top_text = top_lines.join("\n");
            assert!(top_text.contains("Codex · 23% left"), "{top_text}");
            assert!(!top_text.contains("Codex · @"), "{top_text}");
            assert!(
                top_text.contains("Registered Codex accounts (Codex session log)"),
                "{top_text}"
            );
            assert!(top_text.contains("@alternate · 88% left"), "{top_text}");
            assert!(top_text.contains("@primary · 23% left"), "{top_text}");
            assert!(top_text.contains("@old-acct · stale"), "{top_text}");
            assert!(
                top_text.contains("@expired-acct · window expired"),
                "{top_text}"
            );
            for hidden_pct_label in ["@old-acct ", "@expired-acct "] {
                let line = top_lines
                    .iter()
                    .find(|l| l.contains(hidden_pct_label))
                    .unwrap();
                assert!(!line.contains('%'), "{line}");
            }
            assert!(!top_text.contains("Activity · * working"), "{top_text}");

            for _ in 0..2 {
                weekly.repaint = false;
                assert_eq!(
                    weekly.key(
                        &Key::Down,
                        &accounts_report,
                        accounts_paint(),
                        size,
                        fresh,
                        false
                    ),
                    WeeklyKey::Handled
                );
                assert!(weekly.repaint);
            }
            let down = frame(
                &accounts_report,
                &nav,
                size,
                &Default::default(),
                true,
                Some(&mut DockSplit::default()),
                None,
                weekly.overlay(WeeklyPhase::Fresh),
            );
            let down_text = render::clean(&down.lines.join("\n"), 4096);
            assert!(
                down_text
                    .contains("Activity · * working · - idle · ? unknown · ~ pending · ! stale"),
                "{down_text}"
            );
            assert!(!down_text.contains("Codex · 23% left"), "{down_text}");

            for _ in 0..2 {
                weekly.repaint = false;
                assert_eq!(
                    weekly.key(
                        &Key::Up,
                        &accounts_report,
                        accounts_paint(),
                        size,
                        fresh,
                        false
                    ),
                    WeeklyKey::Handled
                );
                assert!(weekly.repaint);
            }
            let up = frame(
                &accounts_report,
                &nav,
                size,
                &Default::default(),
                true,
                Some(&mut DockSplit::default()),
                None,
                weekly.overlay(WeeklyPhase::Fresh),
            );
            let up_text = render::clean(&up.lines.join("\n"), 4096);
            assert!(up_text.contains("Codex · 23% left"), "{up_text}");
            assert!(up_text.contains("@alternate · 88% left"), "{up_text}");
        }

        // Same-scope Pending retention preserves scroll back & upper weekly_used while stripping codex_weekly_account;
        // scope change (changed == true) clears weekly.detail.
        let spy = BoardSpy::new("watch-weekly-scope");
        let ledger = spy.ledger();
        let repo = ledger.ancestors().nth(3).unwrap();
        let (answer, responses) = mpsc::channel();
        let worker =
            BoardRefresh::spawn_with(move || Ok(ScopeCollector { answer: responses })).unwrap();
        let mut board = InteractiveRefresh {
            worker,
            quota: Default::default(),
            scope: None,
            phase: RefreshPhase::Pending,
            deferred_refresh: false,
            agy_group_renewal: None,
        };
        let scope = |room: &str| BoardScope {
            key: RefreshKey::new(repo, Some(room), None).unwrap(),
            room: room.into(),
            main: None,
        };
        let (changed, _) = board.update(Ok(scope("room-a")), false, false);
        assert!(changed);
        answer
            .send(Ok(json!({"id":"room-a","closed":false,"participants":[]})))
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !board.worker.ready() {
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        let (changed, initial) = board.update(Ok(scope("room-a")), false, true);
        assert!(!changed && initial.is_some());
        assert_eq!(board.phase, RefreshPhase::Fresh);

        let mut weekly = WeeklyUsage::default();
        assert_eq!(
            weekly.key(
                &Key::Char('w'),
                &accounts_report,
                accounts_paint(),
                size,
                fresh,
                false
            ),
            WeeklyKey::Handled
        );
        let _ = frame(
            &accounts_report,
            &nav,
            size,
            &Default::default(),
            true,
            Some(&mut DockSplit::default()),
            None,
            weekly.overlay(WeeklyPhase::Fresh),
        );
        assert_eq!(
            weekly.key(
                &Key::Down,
                &accounts_report,
                accounts_paint(),
                size,
                fresh,
                false
            ),
            WeeklyKey::Handled
        );
        let retained_back = weekly.detail.as_ref().unwrap().back;
        assert_eq!(retained_back, 1);

        let mut previous = accounts_report;
        previous["codex_weekly_account"] = json!("primary");
        let (changed, _) = board.update(Ok(scope("room-a")), true, false);
        assert!(!changed);
        reset_weekly_scope(&mut weekly, changed);
        assert!(weekly.detail.is_some());
        let pending_snap = Snapshot {
            status: 0,
            report: RefreshPhase::Fresh
                .report(json!({"room":{"id":"room-a"},"collection":{"ok":true}})),
        };
        let pending = board
            .phase
            .frame_report(Some(pending_snap), Some(previous), "room-a");
        assert!(pending.get("codex_weekly_account").is_none());
        assert_eq!(pending["provider_status"]["codex"]["weekly_used"], 77);
        assert_eq!(pending["provider_accounts"]["codex"]["account"], "primary");
        let upper = frame(
            &pending,
            &nav,
            size,
            &Default::default(),
            true,
            Some(&mut DockSplit::default()),
            None,
            None,
        );
        assert!(
            render::clean(&upper.lines[accounts_hit.row], 4096).contains("Codex 23%"),
            "{:#?}",
            upper.lines
        );
        let pending_overlay = frame(
            &pending,
            &nav,
            size,
            &Default::default(),
            true,
            Some(&mut DockSplit::default()),
            None,
            weekly.overlay(WeeklyPhase::Pending),
        );
        assert!(
            pending_overlay
                .lines
                .iter()
                .all(|line| !line.contains("Refreshing Board"))
        );
        assert_eq!(weekly.detail.as_ref().unwrap().back, retained_back);

        let (changed, _) = board.update(Ok(scope("room-b")), false, false);
        assert!(changed);
        reset_weekly_scope(&mut weekly, changed);
        assert!(weekly.detail.is_none());
    }

    #[test]
    fn reader_only_uses_board_rows_when_open() {
        let report = serde_json::json!({"collection": {"ok": true}});
        let nav = Default::default();
        let read = Default::default();
        let mut chat = super::ChatDock {
            title: "AGY · Recent chat".into(),
            history: "\x1b[31m한글 reply\x1b[0m ".repeat(40),
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        };
        let mut split = DockSplit::default();
        for size @ (columns, height) in [(38, 12), (80, 17), (80, 23), (140, 90)] {
            let rows = split.rows(height);
            let divider = height - rows;
            let idle = super::frame(
                &report,
                &nav,
                size,
                &read,
                true,
                Some(&mut split),
                None,
                None,
            );
            assert_eq!(split.painted_rows, 0);
            assert_eq!(
                split.mouse(
                    &crate::input::Key::Click(1, (divider + 1) as u16),
                    size,
                    size
                ),
                None
            );
            let opened = super::frame(
                &report,
                &nav,
                size,
                &read,
                true,
                Some(&mut split),
                Some(&mut chat),
                None,
            );
            assert!(idle.lines.iter().all(|row| !row.contains("Details")
                && !row.contains("Select an orchestrator")
                && !row.contains("AGY · Recent chat")));
            assert_eq!(split.painted_rows, rows);
            assert!(opened.lines[divider].contains("AGY · Recent chat"));
            assert!(opened.lines[divider + 1].contains("한글"));
            assert!(
                opened.lines[divider..]
                    .iter()
                    .all(|row| { !row.contains(['│', '╭', '╮', '╰', '╯']) })
            );
            assert_eq!(opened.lines.len(), height);
            assert!(
                opened.lines.iter().all(|row| {
                    crate::render::width(&crate::render::clean(row, 4096)) <= columns
                })
            );
        }
        assert_eq!(
            history_rows("\x1b[31mOK  \x1b[0m   ", 2, true),
            rows(&["\x1b[31mOK\x1b[0m"])
        );
    }

    #[test]
    fn divider_drag_requires_a_press_and_clamps_to_the_panel() {
        use crate::input::Key;
        let mut split = super::DockSplit::default();
        let size = (100, 36);
        assert_eq!(split.paint(36, true), 12);
        assert_eq!(split.mouse(&Key::Drag(18), size, size), Some(false));
        assert_eq!(split.mouse(&Key::Click(6, 24), size, size), None);
        assert_eq!(split.mouse(&Key::Click(6, 25), size, size), Some(false));
        assert_eq!(split.mouse(&Key::Drag(18), size, size), Some(true));
        assert_eq!(split.rows(36), 19);
        assert_eq!(split.mouse(&Key::Drag(1), size, size), Some(true));
        assert_eq!(split.rows(36), 20);
        assert_eq!(split.mouse(&Key::Release(36), size, size), Some(true));
        assert_eq!(split.rows(36), 4);
        assert_eq!(split.paint(36, true), 4);
        assert_eq!(split.mouse(&Key::Drag(20), size, size), Some(false));
        assert_eq!(split.mouse(&Key::Click(6, 33), size, size), Some(false));
        assert_eq!(split.mouse(&Key::Drag(20), size, (100, 34)), Some(false));
        assert_eq!(split.rows(36), 4);
    }
    #[test]
    fn codex_board_click_and_enter_open_only_the_painted_native_chat() {
        use super::*;
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        let native = "native-codex-1234567890abcdef";
        let report = serde_json::json!({
            "collection": {"ok":true},
            "provider_sessions": ["codex"],
            "room": {"participants":[{"participant":"call-worker","role":"worker","state":"working","joined":true}]},
            "native_children": [{
                "participant":native,"native_child":true,"provider":"codex","role":"subagent",
                "title":"Evidence review","state":"working","attempt":"native-generation","native_answer":"preview must not be used"
            }]
        });
        let spy = BoardSpy::new("routing");
        let session = commands::Session::new();
        spy.main_active.set(true);
        let observations = spy.observations(native, TURN);
        let mut nav = navigation::Navigation {
            view: crate::layout::View::Detail,
            ..Default::default()
        };
        let mut fence = SelectionFence::None;
        let mut chat = None;
        let calls = render::visible(&report, &nav);
        let native_index = calls.iter().position(native_chat_child).unwrap();
        let board = frame(
            &report,
            &nav,
            (160, 45),
            &Default::default(),
            false,
            Some(&mut DockSplit::default()),
            None,
            None,
        );
        let (row, hit) = board
            .worker_rows
            .iter()
            .find(|(_, hit)| hit.0 == native)
            .unwrap();
        let click = input::Key::Click(1, u16::try_from(row + 1).unwrap());
        let open = |call: &Value| {
            chat_dock(call, &observations, |binding| {
                spy.panel().open_chat_with(
                    binding,
                    None,
                    session.attempt_cache(),
                    |_, terminal, _| {
                        assert!(terminal.contains(TURN));
                        Some("verified transcript".into())
                    },
                )
            })
        };
        spy.main_active.set(false);
        assert!(route_chat_open(
            &click,
            Some(hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            open,
        ));
        assert_eq!(nav.selected, native_index);
        assert_eq!(
            chat.as_ref().map(|dock| dock.history.as_str()),
            Some("verified transcript"),
            "focus from Main to Board must preserve the painted child",
        );

        spy.main_active.set(true);
        let focused_main = spy.observations(native, TURN);
        assert!(!refresh_chat(&mut chat, &focused_main, false));
        assert_eq!(chat.as_ref().unwrap().history, "verified transcript");
        spy.main_active.set(false);
        assert!(route_chat_open(
            &input::Key::Enter,
            None,
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            open,
        ));
        assert_eq!(chat.as_ref().unwrap().history, "verified transcript");
        assert_eq!(spy.capture_count.get(), 4);
        assert!(!refresh_chat(&mut chat, &observations, false));

        let rebound = spy.observations(native, NEW_TURN);
        assert!(refresh_chat(&mut chat, &rebound, false));
        assert!(chat.is_none());
        for key in [click.clone(), input::Key::Enter] {
            // Even with the same painted child, a real dock must be left alone.
            spy.docked.set(true);
            chat = Some(ChatDock {
                title: "prior".into(),
                history: "prior".into(),
                back: 0,
                chat: None,
                agy_locator: None,
                main: None,
            });
            assert!(route_chat_open(
                &key,
                Some(hit),
                &calls,
                &mut nav,
                &mut fence,
                &mut chat,
                open,
            ));
            assert!(chat.is_none());
            assert_eq!(nav.notice.as_deref(), Some("Transcript unavailable"));
        }
        assert_eq!(spy.capture_count.get(), 4);
        spy.docked.set(false);

        // A stale paint must not fall through to a sibling or provider header.
        let mut stale_hit = hit.clone();
        stale_hit.1 = "old-generation".into();
        assert!(route_chat_open(
            &click,
            Some(&stale_hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            |_| panic!("stale painted identity opened"),
        ));
        assert_eq!(nav.notice.as_deref(), Some("Transcript unavailable"));

        let others = calls
            .iter()
            .filter(|row| !native_chat_child(row))
            .cloned()
            .collect::<Vec<_>>();
        reconcile_selection(&calls, &others, &mut nav, &mut fence);
        assert!(fence == SelectionFence::Native);
        assert!(route_chat_open(
            &input::Key::Enter,
            None,
            &others,
            &mut nav,
            &mut fence,
            &mut chat,
            |_| panic!("missing native selection fell through"),
        ));
        assert_eq!(nav.notice.as_deref(), Some("Transcript unavailable"));
        assert_eq!(spy.capture_count.get(), 4);
    }

    #[test]
    fn carried_board_report_drops_private_claude_weekly() {
        use super::{RefreshPhase, clear_private_weekly, clear_rejected_native, idle_report};
        use oms_panel_core::state::Snapshot;
        use serde_json::{Value, json};

        let mut previous = idle_report("room-fixture");
        previous["provider_status"] = json!({"codex":{"weekly_used":14}});
        previous["provider_accounts"] =
            json!({"codex":{"account":"acct-b","pinned":false,"verification":"verified"}});
        previous["claude_local_weekly"] = json!({"weekly_used":14});
        previous["codex_weekly_account"] = json!("acct-b");
        previous["agy_footer_weekly"] = json!({"remaining":"99%"});
        let slots = json!({"used":10,"limit":10,"active":0,"awaiting":8,"unconfirmed":2});
        previous["board_write_slots"] = slots.clone();
        previous["provider_activity"] =
            json!({"antigravity":{"children":[{"role":"Worker","title":"Draft notes"}]}});

        let pending_snap = Snapshot {
            status: 0,
            report: idle_report("room-fixture"),
        };
        for report in [
            RefreshPhase::Fresh.frame_report(None, Some(previous.clone()), "room-fixture"),
            RefreshPhase::Pending.frame_report(None, Some(previous.clone()), "room-fixture"),
            RefreshPhase::Pending.frame_report(
                Some(pending_snap),
                Some(previous.clone()),
                "room-fixture",
            ),
            RefreshPhase::Failed("boom".into()).frame_report(
                None,
                Some(previous.clone()),
                "room-fixture",
            ),
            {
                let mut held =
                    oms_panel_core::state::hold_state(&previous, &json!({"error":"boom"}));
                clear_private_weekly(&mut held);
                held
            },
        ] {
            assert!(report.get("claude_local_weekly").is_none());
            assert!(report.get("codex_weekly_account").is_none());
            assert!(report.get("agy_footer_weekly").is_none());
            assert_eq!(report["provider_status"]["codex"]["weekly_used"], 14);
            assert_eq!(report["provider_accounts"]["codex"]["account"], "acct-b");
        }
        // Only the same scope's last healthy read carries the AGY roster into a
        // pending frame; a scope change clears `previous` before this projection.
        let pending = |sampled: bool, previous: Option<serde_json::Value>| {
            let snapshot = sampled.then(|| Snapshot {
                status: 0,
                report: idle_report("room-fixture"),
            });
            RefreshPhase::Pending.frame_report(snapshot, previous, "room-fixture")
        };
        let roster = |report: &serde_json::Value| {
            report["provider_activity"]["antigravity"]["children"].clone()
        };
        for sampled in [false, true] {
            let carried = pending(sampled, Some(previous.clone()));
            assert_eq!(roster(&carried)[0]["title"], "Draft notes");
            assert_eq!(
                roster(&pending(sampled, Some(carried)))[0]["title"],
                "Draft notes"
            );
            assert!(roster(&pending(sampled, None)).is_null());
        }
        // Only a healthy same-scope held frame keeps its write slot evidence.
        let held = pending(false, Some(previous.clone()));
        assert_eq!(held["board_write_slots"], slots);
        assert_eq!(pending(false, Some(held))["board_write_slots"], slots);
        assert_eq!(pending(false, None)["board_write_slots"], Value::Null);
        assert_eq!(
            pending(true, Some(previous.clone()))["board_write_slots"],
            Value::Null
        );
        // A failed or stale frame hides its roster; pending must not relabel it.
        let failed = RefreshPhase::Failed("boom".into()).frame_report(
            None,
            Some(previous.clone()),
            "room-fixture",
        );
        let mut stale = previous.clone();
        stale["capacity_stale"] = true.into();
        assert_eq!(failed["board_write_slots"], Value::Null);
        for unhealthy in [failed.clone(), stale] {
            for sampled in [false, true] {
                let replay = pending(sampled, Some(unhealthy.clone()));
                assert!(roster(&replay).is_null(), "pending replayed failed roster");
                assert_eq!(replay["board_write_slots"], Value::Null);
                let next = pending(sampled, Some(replay));
                assert!(roster(&next).is_null());
                assert_eq!(next["board_write_slots"], Value::Null);
            }
        }
        let mut sampled = idle_report("room-fixture");
        sampled["provider_activity"] =
            json!({"antigravity":{"children":[{"role":"Researcher","title":"Fresh notes"}]}});
        let fresh = RefreshPhase::Fresh.frame_report(
            Some(Snapshot {
                status: 0,
                report: sampled,
            }),
            Some(failed),
            "room-fixture",
        );
        assert_eq!(roster(&fresh)[0]["title"], "Fresh notes");
        // The follow-up remains pending after this same-scope result contradicts
        // the previously proved account. Its carried quota must disappear.
        let mut contradicted = RefreshPhase::Pending.frame_report(
            Some(Snapshot {
                status: 0,
                report: idle_report("room-fixture"),
            }),
            Some(previous),
            "room-fixture",
        );
        clear_rejected_native(&mut contradicted);
        assert_eq!(contradicted["collection"]["pending"], true);
        for key in [
            "provider_activity",
            "provider_status",
            "provider_accounts",
            "codex_weekly_account",
            "claude_local_weekly",
            "agy_footer_weekly",
        ] {
            assert!(
                contradicted.get(key).is_none(),
                "held {key} survived rejected proof"
            );
        }
    }

    #[test]
    fn coalesced_pending_paints_only_the_current_native_activity() {
        use super::*;
        use serde_json::json;

        let room = "room-fixture";
        let mut observed = idle_report(room);
        observed["provider_sessions"] = json!(["codex", "claude"]);
        observed["provider_activity"] = json!({"codex":"idle"});
        let nav = navigation::Navigation::default();
        let first = RefreshPhase::Pending.frame_report(None, None, room);
        assert!(first["provider_activity"].is_null());
        assert!(!RefreshPhase::Pending.accepts_native_activity(false, false));
        let observation = |phase: &RefreshPhase, has_reading, scope_changed| {
            let mut report = phase.frame_report(None, Some(observed.clone()), room);
            let observed_here = phase.accepts_native_activity(has_reading, scope_changed);
            if has_reading && !scope_changed {
                let current = json!({
                    "provider_sessions": ["codex", "claude"],
                    "provider_activity": {"codex":"working"},
                });
                replace_native_projection(
                    &mut report,
                    NATIVE_FIELDS.map(|key| current.get(key).cloned()),
                );
            }
            if phase.pending() {
                assert_eq!(report["collection"]["pending"], true);
            }
            (report, observed_here)
        };
        let weekly_line = |report: &Value, nav: &navigation::Navigation, size, observed_here| {
            let frame = frame_with_native_activity(
                report,
                nav,
                size,
                &Default::default(),
                false,
                None,
                None,
                None,
                observed_here,
            );
            let hit = frame.weekly_usage.unwrap();
            frame.lines[hit.row].clone()
        };

        let (report, observed_here) = observation(&RefreshPhase::Pending, true, false);
        let accepted = weekly_line(&report, &nav, (120, 25), observed_here);
        assert!(
            observed_here && accepted.contains("Codex — · working"),
            "{accepted}"
        );
        assert!(accepted.contains("Claude — · unknown"), "{accepted}");

        let mut moved = nav.clone();
        assert!(!key_checks_scope(&input::Key::Down, false, false, false));
        assert_eq!(
            moved.apply(&input::Key::Down, 2, 10),
            navigation::Action::None,
        );
        assert_eq!(moved.selected, 1);
        let same_scope = InputScope {
            current: true,
            ..Default::default()
        };
        let redraw_current = observed_here && same_scope.accepts_native_activity_redraw(false);
        for size in [(120, 25), (145, 30)] {
            let redrawn = weekly_line(&report, &moved, size, redraw_current);
            assert!(redrawn.contains("Codex — · working"), "{redrawn}");
        }
        let checkout_changed = InputScope {
            current: true,
            checkout_repaint: true,
            ..Default::default()
        };
        for (scope, recollect) in [
            (&same_scope, true),
            (&checkout_changed, false),
            (&InputScope::default(), false),
        ] {
            let mut still_current = redraw_current;
            still_current &= scope.accepts_native_activity_redraw(recollect);
            let invalidated = weekly_line(&report, &moved, (120, 25), still_current);
            assert!(invalidated.contains("Codex — · pending"), "{invalidated}");
            still_current &= same_scope.accepts_native_activity_redraw(false);
            let later_resize = weekly_line(&report, &moved, (145, 30), still_current);
            assert!(later_resize.contains("Codex — · pending"), "{later_resize}");
        }

        assert_eq!(
            allow_board_action(
                &mut nav.clone(),
                navigation::Action::Open,
                true,
                RefreshPhase::Pending == RefreshPhase::Fresh,
            ),
            navigation::Action::None,
        );

        for (phase, has_reading, scope_changed) in [
            (RefreshPhase::Pending, false, false),
            (RefreshPhase::Pending, true, true),
            (RefreshPhase::RetryingAfterFailure, false, false),
        ] {
            let (report, observed_here) = observation(&phase, has_reading, scope_changed);
            let carried = weekly_line(&report, &nav, (120, 25), observed_here);
            assert!(
                !observed_here && carried.contains("Codex — · pending"),
                "{carried}"
            );
        }
        let (report, observed_here) =
            observation(&RefreshPhase::Failed("fixture".into()), false, false);
        let failed = weekly_line(&report, &nav, (120, 25), observed_here);
        assert!(
            !observed_here && failed.contains("Codex — · stale"),
            "{failed}"
        );
    }

    #[test]
    fn event_backed_idle_refresh_preserves_five_second_recovery() {
        use super::{EVENT_BACKED_FALLBACK, REFRESH_INTERVAL, refresh_interval};

        assert_eq!(refresh_interval(true, true, false), EVENT_BACKED_FALLBACK);
        for (wake, fresh, retry) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert_eq!(refresh_interval(wake, fresh, retry), REFRESH_INTERVAL);
        }
    }

    #[test]
    fn main_checkout_proof_is_carried_only_for_unchanged_owned_main_within_bounds() {
        use super::{
            BoardScope, CHECKOUT_LAST_KNOWN_MAX_AGE, CHECKOUT_LAST_KNOWN_MAX_FAILURES, LiveMain,
            MainCheckout, RefreshKey, allow_board_action, navigation, render,
        };
        use serde_json::json;
        use std::collections::BTreeMap;
        use std::time::{Duration, Instant};

        let repo = std::env::current_dir().unwrap().canonicalize().unwrap();
        let make_scope = |room: &str, attempt: &str, launch: &str| BoardScope {
            key: RefreshKey::new(&repo, Some(room), Some(attempt))
                .unwrap()
                .with_member(Some("main-member"))
                .unwrap()
                .with_launch_path(Some(launch.to_owned())),
            room: room.into(),
            main: Some(LiveMain {
                attempt: attempt.into(),
                member: "main-member".into(),
                owner: "codex".into(),
                session: "$1".into(),
                window: "@1".into(),
                pane: "%1".into(),
                pid: 77,
                birth: "boot:7".into(),
            }),
        };
        let base_report = |room: &str, attempt: &str, pending: bool| {
            json!({
                "collection": {"ok": true, "pending": pending},
                "room": {
                    "id": room,
                    "closed": false,
                    "successor": null,
                    "owner": "main-member",
                    "participants": [{
                        "participant": "main-member",
                        "role": "main",
                        "provider": "codex",
                        "attempt": attempt,
                        "state": "working",
                        "joined": true,
                    }]
                },
                "room_attempts": {
                    attempt: {
                        "attempt_id": attempt,
                        "tool": "panel-main",
                        "provider": "codex",
                        "state": "working",
                        "terminal": false,
                        "refs": {
                            "panel_room_id": room,
                            "panel_role": "main",
                            "panel_room_participant": "main-member",
                        }
                    }
                }
            })
        };

        let scope = make_scope("room-one", "main-1", "/tmp/wt-main");
        let t0 = Instant::now();
        let mut checkout = MainCheckout::default();

        // 1. Fresh accepted report records the verified checkout proof.
        let mut fresh = base_report("room-one", "main-1", false);
        fresh["board_checkout"] = json!({
            "attempt": "main-1",
            "branch": "oms/review",
            "worktree": "wt-main",
            "detached": false,
        });
        checkout.project(Some(&scope), true, false, &mut fresh, t0);
        assert!(fresh.get("board_checkout_last_known").is_none());

        // 2. Ordinary pending refresh for unchanged owned Main carries display-only proof.
        let mut pending = base_report("room-one", "main-1", true);
        checkout.project(
            Some(&scope),
            false,
            false,
            &mut pending,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(pending["board_checkout_last_known"]["display_only"], true);
        assert_eq!(pending["board_checkout_last_known"]["room"], "room-one");
        assert_eq!(pending["board_checkout_last_known"]["attempt"], "main-1");
        let rendered =
            render::frame(&pending, &Default::default(), 100, 20, &BTreeMap::new()).join("\n");
        assert!(
            rendered.contains("Branch oms/review · Worktree wt-main"),
            "{rendered}"
        );
        // Action authority remains unchanged (denied while pending / !row_authority).
        let mut nav = navigation::Navigation::default();
        assert_eq!(
            allow_board_action(&mut nav, navigation::Action::SwitchMain, true, false),
            navigation::Action::None
        );

        // 3. Confirmed checkout change clears proof immediately.
        let mut changed_checkout = base_report("room-one", "main-1", true);
        checkout.project(
            Some(&scope),
            false,
            true,
            &mut changed_checkout,
            t0 + Duration::from_secs(3),
        );
        assert!(changed_checkout.get("board_checkout_last_known").is_none());
        let rendered = render::frame(
            &changed_checkout,
            &Default::default(),
            100,
            20,
            &BTreeMap::new(),
        )
        .join("\n");
        assert!(rendered.contains("Checkout unknown"), "{rendered}");

        // 4. Launch path change or Main replacement clears proof immediately.
        checkout.project(Some(&scope), true, false, &mut fresh, t0);
        let moved_launch = make_scope("room-one", "main-1", "/tmp/wt-other");
        let mut moved_pending = base_report("room-one", "main-1", true);
        checkout.project(
            Some(&moved_launch),
            false,
            false,
            &mut moved_pending,
            t0 + Duration::from_secs(1),
        );
        assert!(moved_pending.get("board_checkout_last_known").is_none());

        checkout.project(Some(&scope), true, false, &mut fresh, t0);
        let replaced_main = make_scope("room-one", "main-2", "/tmp/wt-main");
        let mut replaced_pending = base_report("room-one", "main-2", true);
        checkout.project(
            Some(&replaced_main),
            false,
            false,
            &mut replaced_pending,
            t0 + Duration::from_secs(1),
        );
        assert!(replaced_pending.get("board_checkout_last_known").is_none());

        // 5. Missing or malformed metadata clears/refuses proof.
        for bad in [
            json!(null),
            json!({"attempt": "main-1", "branch": "oms/review", "worktree": "../escape", "detached": false}),
            json!({"attempt": "main-1", "branch": "bad branch", "worktree": "wt-main", "detached": false}),
            json!({"attempt": "main-1", "branch": "oms/review", "worktree": "wt-main", "detached": "yes"}),
            json!({"attempt": "other-main", "branch": "oms/review", "worktree": "wt-main", "detached": false}),
        ] {
            checkout.project(Some(&scope), true, false, &mut fresh, t0);
            let mut malformed = base_report("room-one", "main-1", false);
            malformed["board_checkout"] = bad;
            checkout.project(
                Some(&scope),
                true,
                false,
                &mut malformed,
                t0 + Duration::from_secs(1),
            );
            let mut after = base_report("room-one", "main-1", true);
            checkout.project(
                Some(&scope),
                false,
                false,
                &mut after,
                t0 + Duration::from_secs(2),
            );
            assert!(after.get("board_checkout_last_known").is_none());
        }

        // 6. No unbounded trust after repeated sampling failures or age expiry.
        checkout.project(Some(&scope), true, false, &mut fresh, t0);
        for step in 0..CHECKOUT_LAST_KNOWN_MAX_FAILURES {
            let mut failed = base_report("room-one", "main-1", false);
            failed["collection"] = json!({"ok": false, "error": "sample failed"});
            checkout.project(
                Some(&scope),
                false,
                false,
                &mut failed,
                t0 + Duration::from_secs(u64::from(step) + 1),
            );
            assert!(failed.get("board_checkout_last_known").is_none());
        }
        let mut after_failures = base_report("room-one", "main-1", true);
        checkout.project(
            Some(&scope),
            false,
            false,
            &mut after_failures,
            t0 + Duration::from_secs(5),
        );
        assert!(
            after_failures.get("board_checkout_last_known").is_none(),
            "proof must clear after bounded consecutive failures"
        );

        checkout.project(Some(&scope), true, false, &mut fresh, t0);
        let mut expired = base_report("room-one", "main-1", true);
        checkout.project(
            Some(&scope),
            false,
            false,
            &mut expired,
            t0 + CHECKOUT_LAST_KNOWN_MAX_AGE,
        );
        assert!(
            expired.get("board_checkout_last_known").is_none(),
            "proof must expire after bounded age window"
        );
    }

    #[test]
    fn main_switch_requires_separate_confirmation_for_the_exact_scope() {
        use super::*;
        use crate::orchestrator::account_binding::test_support::TempDir;
        use input::Key;
        use std::os::unix::fs::PermissionsExt;
        let repo = std::env::current_dir().unwrap().canonicalize().unwrap();
        let staged = TempDir::new("board-main-switch");
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let payload = staged.0.join("target/oms-native").join(revision);
        std::fs::create_dir_all(&payload).unwrap();
        let executable = payload.join("oms");
        std::fs::write(&executable, b"fixture executable").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let scope = BoardScope {
            key: RefreshKey::new(&repo, Some("room-one"), Some("attempt-old"))
                .unwrap()
                .with_member(Some("main-member"))
                .unwrap(),
            room: "room-one".into(),
            main: Some(LiveMain {
                attempt: "attempt-old".into(),
                member: "main-member".into(),
                owner: "codex".into(),
                session: "$1".into(),
                window: "@1".into(),
                pane: "%1".into(),
                pid: 52,
                birth: "start-one".into(),
            }),
        };
        let make_menu = || {
            let target = crate::native_panel::MainSwitchTarget {
                harness: "agy",
                model: "gemini-4-carbon-high".into(),
            };
            let menu = navigation::MainSwitchMenu {
                current: "codex".into(),
                choices: vec!["agy · gemini-4-carbon-high".into()],
                selected: None,
            };
            (
                MainSwitch {
                    selection: Some((scope.clone(), vec![target])),
                    ..Default::default()
                },
                navigation::Navigation {
                    main_switch: Some(menu),
                    ..Default::default()
                },
            )
        };
        let refuse = |_: &BoardScope, _: &crate::native_panel::MainSwitchTarget| {
            panic!("cancelled or stale switch reached the dispatcher")
        };
        let report = json!({
            "project_name": "proj",
            "generated_at": 1_700_000_037,
            "collection": {"ok": true},
            "room": {
                "id": "room-one",
                "closed": false,
                "main": "main-member",
                "participants": [
                    {"participant": "earlier", "role": "main", "provider": "claude", "model": "opus", "state": "working", "created_at": "1"},
                    {"participant": "main-member", "attempt": "attempt-old", "role": "main", "provider": "codex", "model": "gpt-x", "state": "working", "joined": true}
                ]
            },
            "room_attempts": {
                "attempt-old": {
                    "attempt_id": "attempt-old",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "working",
                    "terminal": false,
                    "refs": {
                        "panel_host": "tmux",
                        "panel_harness": "codex",
                        "panel_room_id": "room-one",
                        "panel_room_participant": "main-member",
                        "panel_role": "main"
                    }
                }
            }
        });
        let read = std::collections::BTreeMap::new();
        let mut board_nav = navigation::Navigation::default();
        let board = frame(
            &report,
            &board_nav,
            (100, 20),
            &read,
            false,
            None,
            None,
            None,
        );
        let zone = board
            .main_switch
            .expect("Switch Main hit zone on Board with pinned Main");
        let click_x = zone.start as u16;
        let click_y = (zone.row + 1) as u16;
        let action = click_action(
            click_x,
            click_y,
            (100, 20),
            (100, 20),
            false,
            board.main_switch,
            &board.provider_rows,
        );
        assert_eq!(action, navigation::Action::SwitchMain);
        assert_eq!(
            allow_board_action(&mut board_nav, action, true, true),
            navigation::Action::SwitchMain
        );
        assert_eq!(
            allow_board_action(&mut board_nav, action, true, false),
            navigation::Action::None
        );
        assert_eq!(
            click_action(
                click_x,
                click_y,
                (100, 20),
                (100, 20),
                true,
                board.main_switch,
                &board.provider_rows,
            ),
            navigation::Action::None
        );
        let rows = render::visible(&report, &navigation::Navigation::default());
        let earlier = rows
            .iter()
            .position(|call| crate::text(call, "participant") == "earlier")
            .unwrap();
        let detail_nav = navigation::Navigation {
            detail: true,
            selected: earlier,
            ..Default::default()
        };
        let detail_board = frame(
            &report,
            &detail_nav,
            (100, 20),
            &read,
            false,
            None,
            None,
            None,
        );
        assert!(detail_board.main_switch.is_none());
        assert_eq!(
            click_action(
                click_x,
                click_y,
                (100, 20),
                (100, 20),
                false,
                detail_board.main_switch,
                &detail_board.provider_rows,
            ),
            navigation::Action::None
        );
        let mut dock_split = DockSplit::default();
        let mut dock = ChatDock {
            title: "Worker".into(),
            history: "body".into(),
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        };
        let docked_board = frame(
            &report,
            &navigation::Navigation::default(),
            (100, 14),
            &read,
            false,
            Some(&mut dock_split),
            Some(&mut dock),
            None,
        );
        assert!(docked_board.main_switch.is_none());
        assert_eq!(
            click_action(
                click_x,
                click_y,
                (100, 14),
                (100, 14),
                false,
                docked_board.main_switch,
                &docked_board.provider_rows,
            ),
            navigation::Action::None
        );
        let mut legacy_scope = scope.clone();
        legacy_scope.main.as_mut().unwrap().member = "attempt-old".into();
        legacy_scope.key = legacy_scope.key.with_member(Some("attempt-old")).unwrap();
        let mut legacy = report.clone();
        legacy["room"]["participants"][1]["participant"] = json!("attempt-old");
        let refs = legacy["room_attempts"]["attempt-old"]["refs"]
            .as_object_mut()
            .unwrap();
        refs.remove("panel_host");
        refs.remove("panel_room_participant");
        let blocked = |case_scope: &BoardScope, report: &Value| {
            let mut flow = MainSwitch::default();
            let mut nav = navigation::Navigation::default();
            let error = flow
                .open_with(Some(case_scope), report, &mut nav, |_| {
                    panic!("unavailable Main must not resolve targets")
                })
                .unwrap_err();
            assert!(nav.main_switch.is_none());
            assert!(flow.selection.is_none() && flow.running.is_none());
            error.message
        };
        assert_eq!(
            blocked(&legacy_scope, &legacy),
            "This Main predates managed switching. Hand off work, exit normally, then launch a new managed Main."
        );
        for (path, value, expected) in [
            (
                "/collection/pending",
                json!(true),
                "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
            ),
            (
                "/room/closed",
                json!(true),
                "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
            ),
            (
                "/room_attempts/attempt-old/attempt_id",
                json!("foreign-attempt"),
                "Current Main custody changed or is no longer live; refresh the Board before switching",
            ),
            (
                "/room_attempts/attempt-old/refs/panel_room_id",
                json!("room-foreign"),
                "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
            ),
            (
                "/room_attempts/attempt-old/refs/panel_wrapper_pid",
                json!("52"),
                "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
            ),
            (
                "/room_attempts/attempt-old/refs/panel_host",
                json!("inline"),
                "Current Main is running inline, not in a managed tmux pane. Hand off work and exit normally before launching a managed tmux Main.",
            ),
            (
                "/room_attempts/attempt-old/state",
                json!("done"),
                "Current Main custody changed or is no longer live; refresh the Board before switching",
            ),
            (
                "/room/participants/1/joined",
                json!(false),
                "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
            ),
        ] {
            let mut refused = legacy.clone();
            let (parent, key) = path.rsplit_once('/').unwrap();
            refused.pointer_mut(parent).unwrap()[key] = value;
            assert_eq!(blocked(&legacy_scope, &refused), expected, "{path}");
        }
        for (path, value, expected) in [
            (
                "/room_attempts/attempt-old/refs/panel_host",
                json!("external"),
                "Current Main lacks complete managed tmux custody proof; refresh the Board and verify the existing Main",
            ),
            (
                "/room_attempts/attempt-old/state",
                json!("blocked"),
                "Current Main is blocked; wait until it is working to switch",
            ),
            (
                "/room_attempts/attempt-old/refs/panel_native_scheduling",
                json!("campaign"),
                "Main swap is unavailable during a managed campaign",
            ),
        ] {
            let mut refused = report.clone();
            let (parent, key) = path.rsplit_once('/').unwrap();
            refused.pointer_mut(parent).unwrap()[key] = value;
            assert_eq!(blocked(&scope, &refused), expected, "{path}");
        }
        let expected_refusal = crate::native_panel::check_outgoing_main_swap_harness(None)
            .unwrap_err()
            .message;
        for (owner, harness, expected) in [
            (
                "antigravity",
                Some("agy"),
                "safe automatic exit currently requires a Codex Main composer; hand off work and exit the AGY Main normally before launching a replacement",
            ),
            ("codex", Some("pi"), expected_refusal.as_str()),
            ("codex", Some("unknown"), expected_refusal.as_str()),
            ("codex", None, expected_refusal.as_str()),
        ] {
            let mut case_scope = scope.clone();
            case_scope.main.as_mut().unwrap().owner = owner.into();
            let mut case_report = report.clone();
            let attempt = case_report["room_attempts"]["attempt-old"]
                .as_object_mut()
                .unwrap();
            attempt.insert("provider".into(), json!(owner));
            let refs = attempt["refs"].as_object_mut().unwrap();
            match harness {
                Some(value) => {
                    refs.insert("panel_harness".into(), json!(value));
                }
                None => {
                    refs.remove("panel_harness");
                }
            }
            let mut flow = MainSwitch::default();
            let mut nav = navigation::Navigation::default();
            let error = flow
                .open_with(Some(&case_scope), &case_report, &mut nav, |_| {
                    panic!("unsupported outgoing harness must not resolve targets")
                })
                .unwrap_err();
            assert_eq!(error.message, expected);
            assert!(nav.main_switch.is_none());
            assert!(flow.selection.is_none());
            assert!(flow.running.is_none());
            assert!(!flow.key(
                &Key::Char('1'),
                &mut nav,
                || Some(case_scope.clone()),
                refuse
            ));
            assert!(!flow.key(
                &Key::Char('y'),
                &mut nav,
                || Some(case_scope.clone()),
                refuse
            ));
        }
        let mut opened_flow = MainSwitch::default();
        let mut opened_nav = navigation::Navigation {
            notice: Some("stale".into()),
            ..Default::default()
        };
        opened_flow
            .open_with(Some(&scope), &report, &mut opened_nav, |current| {
                assert_eq!(current, "codex");
                Ok(vec![crate::native_panel::MainSwitchTarget {
                    harness: "agy",
                    model: "gemini-4-carbon-high".into(),
                }])
            })
            .unwrap();
        assert_eq!(opened_nav.main_switch.as_ref().unwrap().current, "codex");
        assert!(opened_nav.notice.is_none());
        assert!(opened_flow.selection.is_some());
        let (mut flow, mut nav) = make_menu();
        let modal = frame(&report, &nav, (100, 20), &read, false, None, None, None);
        assert!(modal.main_switch.is_none());
        assert!(flow.running.is_none());
        assert!(!flow.key(&Key::Click(2, 2), &mut nav, || None, refuse));
        assert!(nav.main_switch.as_ref().unwrap().selected.is_none());
        assert!(!flow.key(&Key::Char('y'), &mut nav, || None, refuse));
        assert!(flow.key(&Key::Char('1'), &mut nav, || None, refuse));
        assert!(!flow.key(&Key::Click(2, 3), &mut nav, || Some(scope.clone()), refuse));
        assert!(flow.running.is_none() && nav.main_switch.is_some());
        let frame = render::main_switch_frame(nav.main_switch.as_ref().unwrap(), 31, 6);
        assert!(frame.lines.iter().any(|line| line.contains("Y Confirm")));
        assert!(frame.worker_rows.is_empty());
        assert!(frame.main_switch.is_none());
        assert!(flow.key(&Key::Escape, &mut nav, || None, refuse));
        assert!(flow.selection.is_none() && nav.main_switch.is_none());
        let (mut flow, mut nav) = make_menu();
        assert!(flow.key(&Key::Char('1'), &mut nav, || None, refuse));
        let mut replaced = scope.clone();
        replaced.main.as_mut().unwrap().birth = "start-two".into();
        assert!(flow.key(&Key::Char('y'), &mut nav, || Some(replaced), refuse));
        assert!(nav.notice.unwrap().contains("Main changed"));
        let (mut flow, mut nav) = make_menu();
        assert!(flow.key(&Key::Char('1'), &mut nav, || None, refuse));
        assert!(flow.key(
            &Key::Char('y'),
            &mut nav,
            || Some(scope.clone()),
            |expected, target| {
                assert_eq!(expected, &scope);
                assert!(
                    main_switch_request(&executable, &staged.0, &repo, expected, target).is_err()
                );
                std::fs::write(payload.join("manifest.json"), b"{}").unwrap();
                assert!(main_switch_request(&executable, &repo, &repo, expected, target).is_err());
                let req = main_switch_request(&executable, &staged.0, &repo, expected, target)?;
                assert_eq!(
                    req.argv[..6],
                    [
                        executable.as_os_str().to_owned(),
                        "--oms-dispatch".into(),
                        staged.0.as_os_str().to_owned(),
                        revision.into(),
                        "--".into(),
                        "panel".into(),
                    ]
                );
                let opts = Options::parse(&req.argv[6..])?;
                assert_eq!(opts.get("room"), Some("room-one"));
                assert_eq!(opts.get("swap-main"), Some("agy"));
                assert_eq!(opts.get("model"), Some("gemini-4-carbon-high"));
                assert_eq!(
                    req.environment,
                    [
                        ("OMS_PANEL_MAIN_ATTEMPT".into(), "attempt-old".into()),
                        ("OMS_PANEL_OWNER".into(), "codex".into()),
                        ("OMS_ROOM_PARTICIPANT".into(), "main-member".into()),
                    ]
                );
                let (done, result) = mpsc::channel();
                done.send(Ok(())).unwrap();
                Ok(result)
            }
        ));
        assert_eq!(
            flow.poll(None, |_| panic!(
                "Codex cannot use the operator exit prompt"
            ))
            .as_deref(),
            Some("Main switch verified"),
        );

        let mut claude = scope.clone();
        claude.main.as_mut().unwrap().owner = "claude".into();
        let mut claude_report = report.clone();
        claude_report["room"]["participants"][1]["provider"] = json!("claude");
        let attempt = &mut claude_report["room_attempts"]["attempt-old"];
        attempt["provider"] = json!("claude");
        let refs = &mut attempt["refs"];
        refs["panel_harness"] = json!("claude");
        refs["panel_wrapper_pid"] = json!("52");
        refs["panel_wrapper_identity"] = json!("start-one");
        refs["panel_native_pid"] = json!("53");
        refs["panel_native_identity"] = json!("start-child");
        let unavailable = !crate::native_panel::claude_assisted_exit_available();
        let claude_card = super::frame(
            &claude_report,
            &navigation::Navigation::default(),
            (100, 20),
            &read,
            false,
            None,
            None,
            None,
        );
        assert_eq!(claude_card.main_switch.is_none(), unavailable);
        if unavailable {
            assert_eq!(
                blocked(&claude, &claude_report),
                crate::native_panel::CLAUDE_ASSISTED_EXIT_UNAVAILABLE,
            );
            return;
        }
        assert_eq!(
            blocked(&claude, &claude_report),
            crate::native_panel::CLAUDE_OPERATOR_EXIT_UPGRADE,
        );
        claude_report["room_attempts"]["attempt-old"]["refs"]
            [crate::native_panel::CLAUDE_OPERATOR_EXIT_REF] =
            json!(crate::native_panel::CLAUDE_OPERATOR_EXIT_VERSION);
        let mut missing = claude_report.clone();
        missing["room_attempts"]["attempt-old"]["refs"]["panel_native_pid"] = Value::Null;
        assert!(blocked(&claude, &missing).contains("verified managed custody"));
        let mut flow = MainSwitch::default();
        let mut nav = navigation::Navigation::default();
        flow.open_with(Some(&claude), &claude_report, &mut nav, |current| {
            assert_eq!(current, "claude");
            Ok(vec![crate::native_panel::MainSwitchTarget {
                harness: "codex",
                model: "fixture-model".into(),
            }])
        })
        .unwrap();
        let (done, result) = mpsc::channel();
        assert!(flow.key(&Key::Char('1'), &mut nav, || None, refuse));
        assert!(flow.key(
            &Key::Char('Y'),
            &mut nav,
            || Some(claude.clone()),
            |expected, selected| {
                assert_eq!(expected, &claude);
                assert_eq!(selected.harness, "codex");
                assert!(
                    main_switch_request(
                        Path::new("/fixture/oms"),
                        &repo,
                        &repo,
                        expected,
                        selected
                    )
                    .is_err()
                );
                let request =
                    main_switch_request(&executable, &staged.0, &repo, expected, selected)?;
                assert_eq!(
                    &request.argv[1..5],
                    &[
                        "--oms-dispatch".into(),
                        staged.0.as_os_str().to_owned(),
                        revision.into(),
                        "--".into(),
                    ]
                );
                let options = Options::parse(&request.argv[6..])?;
                assert_eq!(options.get("room"), Some("room-one"));
                assert_eq!(options.get("swap-main"), Some("codex"));
                Ok(result)
            }
        ));
        assert_eq!(
            nav.notice.as_deref(),
            Some("Preparing Claude Main switch to codex")
        );
        assert!(flow.poll(nav.notice.as_deref(), |_| false).is_none());
        flow.operator_exit.as_mut().unwrap().next_probe = Instant::now();
        assert_eq!(
            flow.poll(nav.notice.as_deref(), |pending| same_footer_scope(
                &pending.scope,
                &claude
            ))
            .as_deref(),
            Some(CLAUDE_EXIT_INSTRUCTION),
        );
        flow.operator_exit.as_mut().unwrap().next_probe = Instant::now();
        claude.main.as_mut().unwrap().birth = "foreign-start".into();
        assert_eq!(
            flow.poll(Some(CLAUDE_EXIT_INSTRUCTION), |pending| same_footer_scope(
                &pending.scope,
                &claude
            ))
            .as_deref(),
            Some("Awaiting verified Main exit"),
        );
        done.send(Ok(())).unwrap();
        assert_eq!(
            flow.poll(None, |_| panic!("Completion takes precedence"))
                .as_deref(),
            Some("Main switch verified"),
        );
        assert!(flow.operator_exit.is_none());
    }

    #[test]
    fn managed_refresh_scope_follows_live_tags_and_ignores_focus() {
        use super::*;
        use crate::orchestrator::codex_chat_fixture::BoardSpy;
        use oms_panel_core::{CommandOutput, PeerExecutor, services::ExecutionRequest};

        struct Tmux<'a> {
            repo: &'a Path,
            room: &'static str,
            attempt: &'static str,
            member: Option<&'static str>,
            board_member: Option<&'static str>,
            owner: &'static str,
            main_first: bool,
            board_tagged: bool,
            launch_path: Option<&'a str>,
        }

        impl PeerExecutor for Tmux<'_> {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                assert_eq!(
                    request.argv,
                    ["tmux", "list-panes", "-t", "%2", "-F", WATCH_PANES]
                );
                let pid = std::process::id();
                let room = self.room;
                let main = self.attempt;
                let member = self.member.unwrap_or(main);
                let board_member = self.board_member.unwrap_or(member);
                let owner = self.owner;
                let repo = self.repo.display();
                let tagged = u8::from(self.board_tagged);
                let board = format!(
                    "$1\t@1\t%2\t{pid}\t0\t{tagged}\t{main}\t%1\t{owner}\t{board_member}\t{room}\t{repo}\t."
                );
                let mut main = format!(
                    "$1\t@1\t%1\t{pid}\t0\t0\t{main}\t%1\t{owner}\t{member}\t{room}\t{repo}\t."
                );
                if let Some(path) = self.launch_path {
                    main.push('\t');
                    main.push_str(path);
                }
                let rows = if self.main_first {
                    [main, board]
                } else {
                    [board, main]
                };
                Ok(CommandOutput {
                    stdout: rows.join("\n").into_bytes(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }

        impl Services for Tmux<'_> {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[std::ffi::OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                panic!("Scope proof must stay in tmux, saw {command:?}")
            }
        }

        let spy = BoardSpy::new("watch-scope");
        let ledger = spy.ledger();
        let repo = ledger.ancestors().nth(3).unwrap();
        let mut target = BoardTarget {
            room: Some("startup-room".into()),
            caller: Some("%2".into()),
            managed: false,
        };
        let stop = AtomicBool::new(false);
        let mut tmux = Tmux {
            repo,
            room: "room-a",
            attempt: "att_11111111111111111111111111111111",
            owner: "codex",
            member: None,
            board_member: None,
            main_first: false,
            board_tagged: true,
            launch_path: None,
        };
        let first = target.sample(repo, &tmux, &stop).unwrap();
        assert_eq!(first.room, "room-a");
        tmux.main_first = true;
        assert_eq!(target.sample(repo, &tmux, &stop).unwrap(), first);
        tmux.member = Some("original-participant");
        let swapped = target.sample(repo, &tmux, &stop).unwrap();
        assert_ne!(swapped, first);
        assert_eq!(
            swapped.main.as_ref().unwrap().member,
            "original-participant"
        );
        let snapshot = Snapshot {
            status: 0,
            report: json!({"room":{"id":"room-a","closed":false,"participants":[
                {"participant":"original-participant","role":"main","provider":"codex","joined":true}
            ]},"room_attempts":{(tmux.attempt):{"attempt_id":tmux.attempt,"provider":"codex","tool":"panel-main","terminal":false,"state":"working","refs":{
                "panel_room_id":"room-a","panel_role":"main","panel_room_participant":"original-participant"
            }}}}),
        };
        assert!(swapped.key.matches(&snapshot));
        assert!(!first.key.matches(&snapshot));
        tmux.board_member = Some("foreign-member");
        assert!(target.sample(repo, &tmux, &stop).is_err());
        tmux.member = None;
        tmux.board_member = None;
        tmux.launch_path = repo.to_str();
        let added = target.sample(repo, &tmux, &stop).unwrap();
        assert_eq!(
            added, first,
            "display evidence must not change Board custody"
        );
        assert!(!added.key.same_launch_path(&first.key));
        tmux.launch_path = Some("invalid\tpath");
        let invalid = target.sample(repo, &tmux, &stop).unwrap();
        assert_eq!(invalid, first);
        assert!(invalid.key.same_launch_path(&first.key));
        tmux.room = "room-b";
        tmux.attempt = "att_22222222222222222222222222222222";
        tmux.owner = "claude";
        let next = target.sample(repo, &tmux, &stop).unwrap();
        assert_ne!(next.key, first.key);
        assert_eq!(next.room, "room-b");
        assert_eq!(next.main.unwrap().owner, "claude");
        tmux.board_tagged = false;
        assert!(
            target.sample(repo, &tmux, &stop).is_err(),
            "after managed proof do not fall back to the startup Room"
        );
    }

    #[test]
    fn codex_main_footer_is_read_once_between_two_identical_proofs() {
        use super::*;
        use crate::orchestrator::codex_chat_fixture::BoardSpy;
        use oms_panel_core::{CommandOutput, PeerExecutor, services::ExecutionRequest};
        use std::cell::{Cell, RefCell};

        const PATH: &str = "/srv/work/repo";
        const LIVE: &str = "› \n\n  GPT-6.2-Sol high · Context 9% left · weekly 13% left · main · ~/fixture · Main [default]\n← for agents · ? for shortcuts";

        struct Tmux<'a> {
            repo: &'a Path,
            owner: &'static str,
            screen: RefCell<String>,
            captured: Cell<bool>,
            // Rewrites the custody rows for the proof after capture.
            drift: Cell<&'static str>,
            calls: RefCell<Vec<String>>,
        }

        impl PeerExecutor for Tmux<'_> {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                let argv: Vec<_> = request.argv.iter().map(|a| a.to_string_lossy()).collect();
                self.calls.borrow_mut().push(argv.join(" "));
                let mut out = CommandOutput {
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                };
                if argv[1] == "capture-pane" {
                    assert_eq!(argv[2..], ["-p", "-t", "%1"], "read-only capture of Main");
                    self.captured.set(true);
                    out.stdout = self.screen.borrow().clone().into_bytes();
                    return Ok(out);
                }
                assert_eq!(argv[1], "list-panes");
                let pid = std::process::id();
                let (mut attempt, mut room, mut dead, mut mpid) =
                    ("att_11111111111111111111111111111111", "room-a", 0, pid);
                let mut launch_path = PATH;
                let repo = self.repo.display();
                let mut owner = self.owner;
                let mut pane = "%1";
                if self.captured.get() {
                    match self.drift.get() {
                        "attempt" => attempt = "att_22222222222222222222222222222222",
                        "room" => room = "room-b",
                        "dead" => dead = 1,
                        "pid" => mpid = 1,
                        "path" => launch_path = "/srv/work/other",
                        "owner" => owner = "claude",
                        "pane" => pane = "%3",
                        _ => {}
                    }
                }
                let board = format!(
                    "$1\t@1\t%2\t{pid}\t0\t1\t{attempt}\t{pane}\t{owner}\t{attempt}\t{room}\t{repo}\t."
                );
                let main = format!(
                    "$1\t@1\t{pane}\t{mpid}\t{dead}\t0\t{attempt}\t{pane}\t{owner}\t{attempt}\t{room}\t{repo}\t.\t{launch_path}"
                );
                out.stdout = [board, main].join("\n").into_bytes();
                Ok(out)
            }
        }

        impl Services for Tmux<'_> {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[std::ffi::OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                panic!("Scope proof must stay in tmux, saw {command:?}")
            }
        }

        let spy = BoardSpy::new("watch-footer");
        let ledger = spy.ledger();
        let repo = ledger.ancestors().nth(3).unwrap();
        let stop = AtomicBool::new(false);
        let tmux = Tmux {
            repo,
            owner: "codex",
            screen: RefCell::new(LIVE.into()),
            captured: Cell::new(false),
            drift: Cell::new(""),
            calls: RefCell::new(vec![]),
        };
        let read = |tmux: &Tmux, screen: &str, drift: &'static str| {
            let mut target = BoardTarget {
                room: Some("startup-room".into()),
                caller: Some("%2".into()),
                managed: false,
            };
            *tmux.screen.borrow_mut() = screen.into();
            tmux.captured.set(false);
            tmux.drift.set(drift);
            let scope = target.sample(repo, tmux, &stop).unwrap();
            tmux.calls.borrow_mut().clear();
            let seen = target.read_footer(repo, tmux, &stop, &scope);
            (scope, seen)
        };

        let (scope, seen) = read(&tmux, LIVE, "");
        let attempt = &scope.main.as_ref().unwrap().attempt;
        assert_eq!(seen.as_deref(), Some("GPT-6.2-Sol"));
        let calls = tmux.calls.borrow().clone();
        assert_eq!(calls.len(), 3, "proof, capture, proof: {calls:?}");
        assert!(calls[0].starts_with("tmux list-panes") && calls[2] == calls[0]);
        assert_eq!(calls[1], "tmux capture-pane -p -t %1");

        // Outside rows and malformed footer fields cannot prove a pane model.
        let keys = "← for agents · ? for shortcuts";
        for screen in [
            format!("{LIVE}\nmore output"),
            LIVE.replace(keys, &format!("buried\n{keys}")),
            LIVE.replace("~/fixture", "not a directory label"),
            LIVE.replace("Main [default]", "Main [default] · extra"),
            LIVE.replace(" · ~/fixture", ""),
            LIVE.replace("main ·", "bad branch ·"),
        ] {
            assert!(read(&tmux, &screen, "").1.is_none(), "{screen}");
        }
        // Identity must be the same before and after the capture.
        for drift in ["attempt", "room", "dead", "pid", "path", "owner", "pane"] {
            let (_, seen) = read(&tmux, LIVE, drift);
            assert!(seen.is_none(), "{drift}");
        }
        let mut footer = MainFooter::default();
        let at = Instant::now();
        let report = || json!({"room":{"id":scope.room},"collection":{"ok":true}});
        let mut fresh = report();
        footer.project(Some(&scope), true, &mut fresh, at, |_| {
            Some("GPT-6.2-Sol".into())
        });
        assert_eq!(
            fresh["main_model_observation"],
            json!({"attempt":attempt,"model":"GPT-6.2-Sol","source":"footer"})
        );
        let mut held = fresh.clone();
        held["collection"]["pending"] = json!(true);
        footer.project(Some(&scope), false, &mut held, at, |_| panic!("held read"));
        assert_eq!(held["main_model_observation"]["source"], "footer-last");
        let mut rollout = report();
        rollout["main_model_observation"] =
            json!({"attempt":attempt,"model":"gpt-rollout","source":"rollout"});
        footer.project(Some(&scope), true, &mut rollout, at, |_| {
            panic!("fresh rollout read")
        });
        assert_eq!(rollout["main_model_observation"]["model"], "gpt-rollout");
        let mut absent = report();
        footer.project(
            Some(&scope),
            true,
            &mut absent,
            at + Duration::from_secs(29),
            |_| panic!("source change bypassed cooldown"),
        );
        assert!(absent["main_model_observation"].is_null());
        footer.project(
            Some(&scope),
            true,
            &mut absent,
            at + FOOTER_INTERVAL,
            |_| None,
        );
        footer.project(
            Some(&scope),
            true,
            &mut absent,
            at + FOOTER_INTERVAL,
            |_| panic!("negative reading bypassed cooldown"),
        );
        let mut moved = scope.clone();
        moved.key = moved
            .key
            .clone()
            .with_launch_path(Some("/srv/work/moved".into()));
        footer.project(
            Some(&moved),
            false,
            &mut fresh,
            at + FOOTER_INTERVAL,
            |_| panic!("moved path was not freshly collected"),
        );
        assert!(fresh["main_model_observation"].is_null());

        // Not Codex: no capture at all.
        let claude = Tmux {
            owner: "claude",
            calls: RefCell::new(vec![]),
            screen: RefCell::new(LIVE.into()),
            captured: Cell::new(false),
            drift: Cell::new(""),
            repo,
        };
        let (_, seen) = read(&claude, LIVE, "");
        assert!(seen.is_none());
        assert!(
            claude
                .calls
                .borrow()
                .iter()
                .all(|c| !c.contains("capture-pane"))
        );
    }

    #[test]
    fn visual_keys_do_not_check_scope_or_touch_tmux() {
        use super::*;
        use input::Key;
        for key in [
            Key::Up,
            Key::Down,
            Key::PageUp,
            Key::PageDown,
            Key::Char('j'),
            Key::Char('k'),
            Key::Char('/'),
            Key::Release(0),
            Key::Drag(0),
            Key::Scroll(3),
            Key::Repaint,
            Key::Tab,
            Key::Home,
            Key::End,
        ] {
            assert!(!key_checks_scope(&key, false, false, false), "{key:?}");
        }
    }

    #[test]
    fn watched_codex_uses_only_accepted_idle_scope_and_hides_held_history() {
        use super::*;
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, TURN};
        use oms_panel_core::{CommandOutput, PeerExecutor};
        use std::sync::mpsc::{self, Receiver, Sender};

        struct Collector {
            rooms: Sender<String>,
            answer: Receiver<Result<Value>>,
        }

        impl PeerExecutor for Collector {
            fn execute(
                &self,
                _: &oms_panel_core::services::ExecutionRequest,
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                panic!("collector must only use native read commands")
            }
        }
        impl Services for Collector {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[std::ffi::OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                let report = match command {
                    NativeCommand::Events => {
                        assert_eq!(args, ["list", "--json"]);
                        json!([{"attempt_id":MAIN,"tool":"panel-main","provider":"codex","terminal":false,"state":"working",
                            "refs":{"panel_role":"main","panel_room_id":ROOM}}])
                    }
                    NativeCommand::Room => {
                        assert_eq!(&args[..2], ["show", "--id"]);
                        assert_eq!(&args[3..6], ["--exact", "--json", "--repo"]);
                        assert_eq!(args[6], repo.as_os_str());
                        self.rooms
                            .send(args[2].to_string_lossy().into())
                            .map_err(Error::new)?;
                        self.answer
                            .recv_timeout(Duration::from_secs(5))
                            .map_err(Error::new)??
                    }
                    _ => panic!("unexpected Board collector command {command:?}"),
                };
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&report).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }

        const ROOM: &str = "room-fixture";
        const MAIN: &str = "att_11111111111111111111111111111111";
        let spy = BoardSpy::new("watch-idle-native");
        let ledger = spy.ledger();
        let repo = ledger.ancestors().nth(3).unwrap();
        let (rooms, requested) = mpsc::channel();
        let (answer, responses) = mpsc::channel();
        let worker = BoardRefresh::spawn_with(move || {
            Ok(Collector {
                rooms,
                answer: responses,
            })
        })
        .unwrap();
        let mut board = InteractiveRefresh {
            worker,
            quota: Default::default(),
            scope: None,
            phase: RefreshPhase::Pending,
            deferred_refresh: false,
            agy_group_renewal: None,
        };
        let scope = |room: &str| BoardScope {
            key: RefreshKey::new(repo, Some(room), Some(MAIN)).unwrap(),
            room: room.into(),
            main: Some(LiveMain {
                attempt: MAIN.into(),
                member: MAIN.into(),
                owner: "codex".into(),
                session: "$1".into(),
                window: "@1".into(),
                pane: "%1".into(),
                pid: 42,
                birth: "birth".into(),
            }),
        };
        let (changed, result) = board.update(Ok(scope(ROOM)), false, false);
        assert!(changed && result.is_none());
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        let pending = board.phase.report(idle_report(ROOM));
        assert_eq!(pending["collection"]["pending"], true);
        assert!(render::calls(&pending).is_empty());
        assert_eq!(pending["board_write_slots"], Value::Null);
        let report_room = json!({"id":ROOM,"closed":false,"participants":[
            {"participant":MAIN,"role":"main","provider":"codex","joined":true},
            {"participant":"call-worker","role":"worker","joined":true,"state":"working"}
        ]});
        let native = "native-codex-child";
        let call =
            json!({"participant":native,"provider":"codex","native_child":true,"role":"subagent"});
        let observations = spy.observations(native, TURN);
        let mut nav = navigation::Navigation::default();
        let mut fence = SelectionFence::None;
        let mut dock = None;
        let keys = [input::Key::Click(1, 1), input::Key::Enter];
        let id = call_identity(&call);
        for key in &keys {
            assert!(route_chat_open(
                key,
                Some(&id),
                std::slice::from_ref(&call),
                &mut nav,
                &mut fence,
                &mut dock,
                |call| {
                    idle_chat_dock(call, &observations, &board, &spy, |_, _, _| {
                        panic!("pending click reached a native read")
                    })
                }
            ));
            assert!(dock.is_none());
        }
        answer.send(Ok(report_room.clone())).unwrap();
        let ready = |board: &mut InteractiveRefresh| {
            let until = Instant::now() + Duration::from_secs(5);
            while !board.worker.ready() {
                assert!(Instant::now() < until, "Board reply did not become ready");
                std::thread::yield_now();
            }
        };
        ready(&mut board);
        let (_, fresh) = board.update(Ok(scope(ROOM)), false, true);
        let fresh = fresh.unwrap().report;
        assert_eq!(board.phase, RefreshPhase::Fresh);
        assert!(!render::calls(&fresh).is_empty());
        for key in &keys {
            assert!(route_chat_open(
                key,
                Some(&id),
                std::slice::from_ref(&call),
                &mut nav,
                &mut fence,
                &mut dock,
                |call| {
                    idle_chat_dock(call, &observations, &board, &spy, |reads, cache, _| {
                        assert_eq!(
                            cache
                                .current_attempt(repo, MAIN, &AtomicBool::new(false))
                                .unwrap()["attempt_id"],
                            MAIN
                        );
                        let fallback = commands::query(
                            reads,
                            NativeCommand::Events,
                            repo,
                            &["show", "--attempt", MAIN, "--json"],
                            &AtomicBool::new(false),
                        );
                        assert!(
                            fallback.is_err(),
                            "stale cache fallback must be refused before Events"
                        );
                        Some("scoped transcript".into())
                    })
                }
            ));
            assert_eq!(dock.as_ref().unwrap().history, "scoped transcript");
        }
        assert_eq!(spy.show_count.get(), 0);
        assert_eq!(spy.list_count.get(), 0);

        assert!(!board.update(Ok(scope(ROOM)), true, false).0);
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        let held = board.phase.frame_report(None, Some(fresh.clone()), ROOM);
        assert!(!render::calls(&held).is_empty());
        assert_eq!(fresh["board_write_slots"]["used"], 0);
        assert_eq!(held["board_write_slots"], fresh["board_write_slots"]);
        assert!(board.chat_cache().is_none());
        assert!(!settle_chat_dock(&mut dock, &observations, &held));
        let paint = |report: &Value, nav: &navigation::Navigation, dock: Option<&mut ChatDock>| {
            frame(
                report,
                nav,
                (120, 40),
                &Default::default(),
                false,
                Some(&mut DockSplit::default()),
                dock,
                None,
            )
            .lines
            .join("\n")
        };
        let pending_paint = paint(&held, &nav, dock.as_mut());
        assert_eq!(pending_paint, paint(&held, &nav, None));
        assert!(!pending_paint.contains("Refreshing Board"));
        assert!(!pending_paint.contains("scoped transcript"));
        assert!(!pending_paint.contains("refresh failed"));
        let mut displayed = Vec::new();
        let mut retained = held;
        retained["native_children"] = json!([call]);
        retained["provider_accounts"] =
            json!({"codex": {"account": "backup", "verification": "verified"}});
        retained["provider_status"] = json!({"codex": {"weekly_used": 14}});
        for label in ["First wake result", "Second wake result"] {
            for _ in 0..4 {
                let (changed, result) = board.update(Ok(scope(ROOM)), true, false);
                assert!(!changed && result.is_none());
            }
            let mut completed_room = report_room.clone();
            completed_room["participants"][1]["label_override"] = json!(label);
            answer.send(Ok(completed_room)).unwrap();
            ready(&mut board);
            let (changed, result) = board.update(Ok(scope(ROOM)), true, true);
            assert!(!changed);
            retained = board.phase.frame_report(result, Some(retained), ROOM);
            let painted = paint(&retained, &nav, dock.as_mut());
            displayed.push((label, painted.contains(label)));
            assert_eq!(retained["native_children"][0]["participant"], native);
            assert_eq!(retained["board_write_slots"], fresh["board_write_slots"]);
            assert_eq!(retained["provider_accounts"]["codex"]["account"], "backup");
            assert_eq!(retained["provider_status"]["codex"]["weekly_used"], 14);
            assert!(!painted.contains("Refreshing Board"));
            assert!(!painted.contains("scoped transcript"));
            assert_eq!(board.phase, RefreshPhase::Pending);
            assert!(board.chat_cache().is_none());
            assert_eq!(
                allow_board_action(
                    &mut nav,
                    navigation::Action::Open,
                    true,
                    board.phase == RefreshPhase::Fresh
                ),
                navigation::Action::None,
            );
            assert_eq!(
                requested.recv_timeout(Duration::from_secs(5)).unwrap(),
                ROOM
            );
        }
        assert_eq!(
            displayed,
            [("First wake result", true), ("Second wake result", true)]
        );

        answer
            .send(Err(Error::new("obsolete wake failure")))
            .unwrap();
        ready(&mut board);
        let (_, failed) = board.update(Ok(scope(ROOM)), true, true);
        assert!(failed.is_none());
        assert_eq!(board.phase, RefreshPhase::RetryingAfterFailure);
        let retrying = board.phase.frame_report(failed, Some(retained), ROOM);
        assert_eq!(retrying["board_write_slots"], Value::Null);
        let painted = paint(&retrying, &nav, dock.as_mut());
        assert!(painted.contains("Second wake result"));
        assert!(!painted.contains("refresh failed"));
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );

        answer.send(Ok(report_room.clone())).unwrap();
        ready(&mut board);
        let (_, restored) = board.update(Ok(scope(ROOM)), false, true);
        let restored = restored.unwrap().report;
        assert!(!settle_chat_dock(&mut dock, &observations, &restored));
        assert!(paint(&restored, &nav, dock.as_mut()).contains("scoped transcript"));

        for action in [
            navigation::Action::Open,
            navigation::Action::Detail,
            navigation::Action::Results,
            navigation::Action::ReadAll,
            navigation::Action::Quota,
            navigation::Action::Council,
        ] {
            nav.detail = true;
            assert_eq!(
                allow_board_action(&mut nav, action, true, false),
                navigation::Action::None
            );
            assert!(!nav.detail);
        }
        for action in [navigation::Action::Provider(0), navigation::Action::Chats] {
            assert_eq!(allow_board_action(&mut nav, action, true, false), action);
            assert_eq!(
                allow_board_action(&mut nav, action, false, false),
                navigation::Action::None
            );
        }
        for action in [
            navigation::Action::Quit,
            navigation::Action::Refresh,
            navigation::Action::Login,
            navigation::Action::None,
        ] {
            assert_eq!(allow_board_action(&mut nav, action, false, false), action);
        }

        assert!(!board.update(Ok(scope(ROOM)), true, false).0);
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        answer.send(Err(Error::new("fixture failure"))).unwrap();
        ready(&mut board);
        assert!(board.update(Ok(scope(ROOM)), false, true).1.is_none());
        assert!(board.chat_cache().is_none());
        let failed = board.phase.report(restored);
        assert!(settle_chat_dock(&mut dock, &observations, &failed));
        assert!(dock.is_none());
        assert!(paint(&failed, &nav, None).contains("refresh failed"));

        assert!(!board.update(Ok(scope(ROOM)), true, false).0);
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        assert!(!board.update(Ok(scope(ROOM)), true, false).0);
        let mut rebound = scope(ROOM);
        assert_eq!(rebound.key, board.scope.as_ref().unwrap().key);
        rebound.main.as_mut().unwrap().birth = "reused PID".into();
        assert!(board.update(Ok(rebound), true, false).0);
        assert!(board.update(Ok(scope(ROOM)), true, false).0);
        assert!(!board.update(Ok(scope(ROOM)), true, false).0);

        // Returning to the original exact tags must still reject the result
        // from before the observed Main process transition.
        let mut pre_rebound = report_room.clone();
        pre_rebound["participants"][1]["label_override"] = json!("Obsolete pre-rebind row");
        answer.send(Ok(pre_rebound)).unwrap();
        ready(&mut board);
        assert!(board.update(Ok(scope(ROOM)), true, true).1.is_none());
        assert!(board.chat_cache().is_none());
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        answer.send(Ok(report_room)).unwrap();
        ready(&mut board);
        assert!(board.update(Ok(scope(ROOM)), false, true).1.is_some());
        assert_eq!(board.phase, RefreshPhase::Fresh);
        assert!(
            !board.worker.busy(),
            "wakes covered by the queued scope must not start another read"
        );
        assert!(board.chat_cache().is_some());

        let new_room = "room-replaced";
        let (changed, replacement) = board.update(Ok(scope(new_room)), false, false);
        assert!(changed && replacement.is_none());
        assert!(board.chat_cache().is_none());
        let cold = board.phase.frame_report(replacement, None, new_room);
        assert_eq!(cold["room"]["id"], new_room);
        assert_eq!(cold["board_write_slots"], Value::Null);
        assert!(render::calls(&cold).is_empty());
    }

    #[test]
    fn codex_board_discards_transcript_after_live_proof_changes() {
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        let spy = BoardSpy::new("proofs");
        let session = super::commands::Session::new();
        let observations = spy.observations("native-codex-child", TURN);
        let binding = observations.chat_binding("native-codex-child").unwrap();
        for change in [&spy.room_closed, &spy.main_terminal] {
            let history =
                spy.panel()
                    .open_chat_with(binding, None, session.attempt_cache(), |_, _, _| {
                        change.set(true);
                        Some("must be discarded".into())
                    });
            assert!(history.is_none());
            change.set(false);
        }
        assert_eq!(spy.capture_count.get(), 2);
        let replaced_turn =
            spy.panel()
                .open_chat_with(binding, None, session.attempt_cache(), |_, _, _| {
                    spy.turn.set(NEW_TURN);
                    Some("old turn transcript".into())
                });
        assert!(replaced_turn.is_none());
        assert_eq!(spy.capture_count.get(), 4);
        spy.turn.set(TURN);

        spy.lead.set("%4");
        let foreign =
            spy.panel()
                .open_chat_with(binding, None, session.attempt_cache(), |_, _, _| {
                    panic!("rebound pane reached the transcript")
                });
        assert!(foreign.is_none());
        assert_eq!(spy.capture_count.get(), 4);
    }

    #[test]
    fn codex_board_reuses_only_a_stable_ledger_main() {
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, TURN};

        let spy = BoardSpy::new("ledger-reuse");
        let session = super::commands::Session::new();
        spy.seed_snapshot(&session);
        let observations = spy.observations("native-codex-child", TURN);
        let binding = observations.chat_binding("native-codex-child").unwrap();

        let open = spy
            .panel()
            .open_chat_with(binding, None, session.attempt_cache(), |_, _, _| {
                Some("owned transcript".into())
            });
        assert_eq!(open.as_deref(), Some("owned transcript"));
        assert_eq!(spy.list_count.get(), 1, "chat must not replay the ledger");
        assert_eq!(
            spy.show_count.get(),
            0,
            "both proofs should reuse a current row"
        );

        for replace in [false, true] {
            spy.main_terminal.set(false);
            spy.seed_snapshot(&session);
            let list_count = spy.list_count.get();
            let show_count = spy.show_count.get();
            let open =
                spy.panel()
                    .open_chat_with(binding, None, session.attempt_cache(), |_, _, _| {
                        let ledger = spy.ledger();
                        if replace {
                            let replacement = ledger.with_extension("new");
                            std::fs::write(&replacement, b"terminal transition\n").unwrap();
                            std::fs::rename(replacement, ledger).unwrap();
                        } else {
                            use std::io::Write;
                            std::fs::OpenOptions::new()
                                .append(true)
                                .open(ledger)
                                .unwrap()
                                .write_all(b"terminal transition\n")
                                .unwrap();
                        }
                        spy.main_terminal.set(true);
                        Some("transcript for no-longer-live Main".into())
                    });
            assert!(open.is_none());
            assert_eq!(spy.list_count.get(), list_count);
            assert_eq!(
                spy.show_count.get(),
                show_count + 1,
                "changed ledger must use exact Events show"
            );
        }
    }

    #[test]
    fn claude_board_click_and_enter_open_only_the_painted_native_chat() {
        use super::*;
        use crate::orchestrator::NativeLocator;
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        let native = crate::native_children::participant_id("claude", "a1b2c3d4e5f60718");
        let report = serde_json::json!({
            "collection": {"ok":true},
            "provider_sessions": ["claude"],
            "room": {"participants":[{"participant":"call-worker","role":"worker","state":"working","joined":true}]},
            "native_children": [{
                "participant":native,"native_child":true,"provider":"claude","role":"subagent",
                "title":"Evidence review","state":"working","attempt":"native-generation","native_answer":"parent summary text"
            }]
        });
        let spy = BoardSpy::new("claude-routing");
        spy.provider.set("claude");
        let session = commands::Session::new();
        let observations = spy.claude_observations(&native, TURN);
        let mut nav = navigation::Navigation {
            view: crate::layout::View::Detail,
            ..Default::default()
        };
        let mut fence = SelectionFence::None;
        let mut chat = None;
        let calls = render::visible(&report, &nav);
        let native_index = calls.iter().position(native_chat_child).unwrap();
        let board = frame(
            &report,
            &nav,
            (160, 45),
            &Default::default(),
            false,
            Some(&mut DockSplit::default()),
            None,
            None,
        );
        let (row, hit) = board
            .worker_rows
            .iter()
            .find(|(_, hit)| hit.0 == native)
            .unwrap();
        let click = input::Key::Click(1, u16::try_from(row + 1).unwrap());
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        let open = |call: &Value| {
            chat_dock(call, &observations, |binding| {
                spy.panel().open_chat_checked(
                    binding,
                    None,
                    session.attempt_cache(),
                    |_, terminal, locator| {
                        assert!(terminal.is_empty());
                        assert!(matches!(locator, NativeLocator::Claude(_)));
                        // The stub stands for the proven reader, which redacts.
                        crate::native_children::chat_tail(&format!("child reply\n{sample}"))
                    },
                    |_, _| Some(TURN.into()),
                )
            })
        };
        assert!(route_chat_open(
            &click,
            Some(hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            open,
        ));
        assert_eq!(nav.selected, native_index);
        let shown = chat.as_ref().unwrap().history.clone();
        assert!(
            shown.contains("child reply") && !shown.contains(&sample),
            "{shown:?}"
        );
        // Bound: the reader's text only, never the parent's.
        assert!(chat.as_ref().unwrap().chat.is_some());
        assert!(!shown.contains("parent summary text") && !shown.contains(super::PARENT_SUMMARY));
        assert!(!refresh_chat(&mut chat, &observations, false));
        assert!(route_chat_open(
            &input::Key::Enter,
            None,
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            open,
        ));
        assert!(chat.is_some());
        let rebound = spy.claude_observations(&native, NEW_TURN);
        assert!(refresh_chat(&mut chat, &rebound, false));
        assert!(chat.is_none());
        // The proven reads are the Codex ones minus the pane capture, and nothing mutates.
        assert_eq!(spy.capture_count.get(), 0);

        // No binding: the worker view with the parent's text, labelled, not a chat.
        let unbound = |call: &Value, seen: &crate::orchestrator::Observations| {
            chat_dock(call, seen, |_| panic!("read without binding")).or_else(|| {
                unbound_summary(call, seen, || {
                    worker_dock(Path::new("."), call, &spy, &AtomicBool::new(false)).ok()
                })
            })
        };
        for key in [click.clone(), input::Key::Enter] {
            chat = None;
            nav.notice = None;
            assert!(route_chat_open(
                &key,
                Some(hit),
                &calls,
                &mut nav,
                &mut fence,
                &mut chat,
                |call| unbound(call, &Default::default()),
            ));
            let dock = chat.as_ref().unwrap();
            assert!(dock.chat.is_none() && nav.notice.is_none());
            assert!(
                dock.history.starts_with(super::PARENT_SUMMARY),
                "{}",
                dock.history
            );
            assert!(dock.history.ends_with("parent summary text"));
            assert!(dock.title.contains("parent summary"));
        }
        // A bound child whose read failed shows no parent text in its place.
        chat = None;
        assert!(route_chat_open(
            &click,
            Some(hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            |call| chat_dock(call, &observations, |_| None).or_else(|| {
                unbound_summary(call, &observations, || panic!("summary for a bound child"))
            }),
        ));
        assert!(chat.is_none());
        assert_eq!(nav.notice.as_deref(), Some("Transcript unavailable"));
        // Codex rows never fall back; their worker view carries no label.
        let mut codex = calls[native_index].clone();
        codex["provider"] = "codex".into();
        assert!(
            unbound_summary(&codex, &Default::default(), || panic!("codex fallback")).is_none()
        );
        let dock = worker_dock(Path::new("."), &codex, &spy, &AtomicBool::new(false)).unwrap();
        assert_eq!(dock.history, "parent summary text");

        // A pending Board hides a private Claude dock like a Codex one.
        let dock = ChatDock {
            title: "t".into(),
            history: "h".into(),
            back: 0,
            chat: Some((
                native.clone(),
                observations.chat_binding(&native).unwrap().clone(),
            )),
            agy_locator: None,
            main: None,
        };
        assert!(private_chat_hidden(
            &serde_json::json!({"collection": {"pending": true}}),
            &dock
        ));
        assert!(!private_chat_hidden(&report, &dock));
    }

    #[test]
    fn unchanged_pane_without_the_binding_drops_the_dock_and_the_read() {
        use super::{ChatDock, settle_chat_dock};
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, TURN};

        let native = "native-claude-child";
        let spy = BoardSpy::new("claude-newer-human");
        spy.provider.set("claude");
        let session = super::commands::Session::new();
        let observations = spy.claude_observations(native, TURN);
        let binding = observations.chat_binding(native).unwrap();
        let mut chat = Some(ChatDock {
            title: "t".into(),
            history: "h".into(),
            back: 0,
            chat: Some((native.into(), binding.clone())),
            agy_locator: None,
            main: None,
        });
        let report = serde_json::json!({"collection": {"ok": true}});
        // Same pane, next observation: a manual message hid the Turn, so no binding.
        assert!(settle_chat_dock(&mut chat, &Default::default(), &report));
        assert!(chat.is_none());

        // The read re-checks the Turn after the reader: no current Turn, no text.
        let open = |current: Option<&'static str>| {
            spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| Some("transcript".into()),
                move |_, _| current.map(Into::into),
            )
        };
        assert_eq!(open(Some(TURN)).as_deref(), Some("transcript"));
        assert!(open(None).is_none());

        // A parent-summary dock is not a chat and survives an unbound refresh.
        let mut summary = Some(ChatDock {
            title: "s".into(),
            history: "h".into(),
            back: 0,
            chat: None,
            agy_locator: None,
            main: None,
        });
        assert!(!settle_chat_dock(
            &mut summary,
            &Default::default(),
            &report
        ));
        assert!(summary.is_some());
    }

    #[test]
    fn claude_board_discards_transcript_after_live_proof_changes() {
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        let native = "native-claude-child";
        let session = super::commands::Session::new();
        let open = |spy: &BoardSpy, binding, turn: &'static str| {
            spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| Some("transcript".into()),
                |_, _| Some(turn.into()),
            )
        };

        // Same read-only query counts as Codex, no pane capture, no mutation
        // (the spy panics on any tmux or service call it does not list).
        let codex = BoardSpy::new("claude-vs-codex");
        let observed = codex.observations("native-codex-child", TURN);
        let binding = observed.chat_binding("native-codex-child").unwrap();
        assert!(
            codex
                .panel()
                .open_chat_with(binding, None, session.attempt_cache(), |_, _, _| Some(
                    "t".into()
                ))
                .is_some()
        );
        let spy = BoardSpy::new("claude-live");
        spy.provider.set("claude");
        let observations = spy.claude_observations(native, TURN);
        let binding = observations.chat_binding(native).unwrap();
        assert_eq!(open(&spy, binding, TURN).as_deref(), Some("transcript"));
        assert_eq!(
            (spy.list_count.get(), spy.show_count.get()),
            (codex.list_count.get(), codex.show_count.get())
        );
        assert_eq!((codex.capture_count.get(), spy.capture_count.get()), (2, 0));

        // The Turn moved between the read and the recheck.
        assert!(open(&spy, binding, NEW_TURN).is_none());
        // The reader never runs when the live proof already fails.
        let refused = |spy: &BoardSpy| {
            spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| panic!("unproven pane reached the transcript"),
                |_, _| panic!("unproven pane reached the Turn"),
            )
        };
        spy.lead.set("%4");
        assert!(refused(&spy).is_none());
        spy.lead.set("%3");
        spy.lead_pid.set(Some(1));
        assert!(refused(&spy).is_none());
        spy.lead_pid.set(None);
        spy.room_closed.set(true);
        assert!(refused(&spy).is_none());
        spy.room_closed.set(false);
        spy.main_terminal.set(true);
        assert!(refused(&spy).is_none());
        spy.main_terminal.set(false);
        spy.docked.set(true);
        assert!(refused(&spy).is_none());
        spy.docked.set(false);
        // A Codex binding is never provable through the Claude lead.
        spy.provider.set("codex");
        assert!(refused(&spy).is_none());
        spy.provider.set("claude");
        assert!(open(&spy, binding, TURN).is_some());

        // The post-reload proof must equal the first: the lead moves during the read.
        let moved = spy.panel().open_chat_checked(
            binding,
            None,
            session.attempt_cache(),
            |_, _, _| {
                spy.lead.set("%4");
                Some("moved".into())
            },
            |_, _| Some(TURN.into()),
        );
        assert!(moved.is_none());
        spy.lead.set("%3");
        // Room or Main proof lost during the read is caught by the second proof alone.
        for lose in [
            &(|| spy.room_closed.set(true)) as &dyn Fn(),
            &(|| spy.main_terminal.set(true)),
        ] {
            let lost = spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| {
                    lose();
                    Some("lost".into())
                },
                |_, _| Some(TURN.into()),
            );
            assert!(lost.is_none());
            spy.room_closed.set(false);
            spy.main_terminal.set(false);
        }
    }

    #[test]
    fn refresh_preserves_participant_and_attempt_after_reorder() {
        let a = serde_json::json!({"participant":"A", "attempt":"1"});
        let b = serde_json::json!({"participant":"B", "attempt":"2"});
        let c = serde_json::json!({"participant":"A", "attempt":"3"});
        assert_eq!(
            super::preserve_selection(&[a.clone(), b.clone()], &[c, b, a], 0),
            2
        );
    }
    #[test]
    fn refresh_selection_fences_replacement_until_enter_or_navigation() {
        let a = serde_json::json!({"participant":"A", "attempt":"1"});
        let b = serde_json::json!({"participant":"B", "attempt":"2"});
        let mut nav = crate::navigation::Navigation {
            detail: true,
            ..Default::default()
        };
        let mut changed = super::SelectionFence::None;
        super::reconcile_selection(&[a, b.clone()], &[b], &mut nav, &mut changed);
        assert!(!nav.detail);
        assert!(changed == super::SelectionFence::Changed);
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut changed, &crate::input::Key::Enter, 1, 1),
            crate::navigation::Action::None
        );
        assert!(nav.notice.as_deref().unwrap().contains("selection changed"));
        assert!(!nav.detail);
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut changed, &crate::input::Key::Enter, 1, 1),
            crate::navigation::Action::Open
        );
        let a = serde_json::json!({"participant":"A", "attempt":"1"});
        let b = serde_json::json!({"participant":"B", "attempt":"2"});
        nav.detail = true;
        super::reconcile_selection(
            &[a.clone(), b.clone()],
            &[b.clone(), a],
            &mut nav,
            &mut changed,
        );
        assert!(nav.detail);
        assert_eq!(nav.selected, 1);
        super::reconcile_selection(
            &[
                b.clone(),
                serde_json::json!({"participant":"A", "attempt":"1"}),
            ],
            &[b],
            &mut nav,
            &mut changed,
        );
        assert!(!nav.detail);
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut changed, &crate::input::Key::Char('f'), 1, 1),
            crate::navigation::Action::None
        );
        assert!(!nav.detail);
    }
    #[test]
    fn call_detail_shortcut_reads_worker_receipts_in_the_short_board() {
        use crate::{input::Key, navigation::Action};
        let report = serde_json::json!({
            "collection":{"ok":true},
            "room":{"id":"room-one","task_id":"task-one","participants":[
                {"participant":"main","role":"main","provider":"codex","state":"working"},
                {"participant":"worker","attempt":"attempt-one","role":"worker",
                 "provider":"codex","task_id":"task-one","state":"working"}
            ]},
            "plan":{"goal":"Review terminal layout","tasks":{"task-one":{"state":"running"}}},
            "room_attempts":{"attempt-one":{
                "attempt_id":"attempt-one","provider":"codex","task_id":"task-one",
                "state":"working","refs":{"panel_room_id":"room-one","panel_room_participant":"worker"},
                "usage":{"tokens":100},"usage_reports":{"tokens":1}
            }},
            "board_receipt_tokens":{"attempt-one":{
                "total":42,"reported":1,"calls":1,"providers":{
                    "codex":{"input":30,"output":12,"cache_read":10,"cache_write":null}
                }
            }}
        });
        let mut nav = crate::navigation::Navigation::default();
        let calls = crate::render::visible(&report, &nav);
        nav.selected = calls
            .iter()
            .position(|call| call["task_id"] == "task-one")
            .unwrap();
        let mut selection = super::SelectionFence::Changed;
        let key = Key::Char('D');
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut selection, &key, calls.len(), 1),
            Action::None
        );
        assert!(!nav.detail);
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut selection, &key, calls.len(), 1),
            Action::None
        );
        assert!(nav.detail);
        let height = 13;
        let frame = super::frame(
            &report,
            &nav,
            (80, height),
            &Default::default(),
            false,
            Some(&mut DockSplit::default()),
            None,
            None,
        );
        let detail = frame.lines.join("\n");
        assert_eq!(frame.lines.len(), height);
        for label in [
            "Call Detail",
            "42 · Receipts",
            "1/1 call totals",
            "In 30 · Out 12 · Cache read 10 · Write ?",
        ] {
            assert!(detail.contains(label), "{detail}");
        }
        assert!(!detail.contains("100 · Events"), "{detail}");
        assert!(frame.worker_rows.is_empty());

        assert_eq!(
            super::apply_selection_key(&mut nav, &mut selection, &Key::Escape, 1, 1),
            Action::None
        );
        assert!(!nav.detail);
        nav.editing = true;
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut selection, &key, 1, 1),
            Action::None
        );
        assert!(!nav.detail);
        assert_eq!(nav.filter, "D");

        // D on Main reads the Main summary, not the generic receipt chrome.
        nav.editing = false;
        nav.filter.clear();
        nav.selected = 0;
        assert_eq!(calls[0]["role"], "main");
        selection = super::SelectionFence::Changed;
        super::apply_selection_key(&mut nav, &mut selection, &key, calls.len(), 1);
        assert!(!nav.detail);
        super::apply_selection_key(&mut nav, &mut selection, &key, calls.len(), 1);
        assert!(nav.detail);
        let detail = super::frame(
            &report,
            &nav,
            (80, 24),
            &Default::default(),
            false,
            Some(&mut DockSplit::default()),
            None,
            None,
        )
        .lines
        .join("\n");
        assert!(!detail.contains("Call Detail"), "{detail}");
        for pair in [
            ["Overall Main", "Esc back"],
            ["Provider", "Codex"],
            ["State", "working"],
        ] {
            assert!(
                detail
                    .lines()
                    .any(|line| pair.iter().all(|part| line.contains(part))),
                "{pair:?}: {detail}"
            );
        }
        super::apply_selection_key(&mut nav, &mut selection, &Key::Escape, 1, 1);
        assert!(!nav.detail);
    }

    #[test]
    fn agy_detail_usage_request_and_interactive_dispatch() {
        use super::{
            BoardScope, BoardTarget, DetailUsageEvidence, DetailUsageFrame, InteractiveRefresh,
            LiveMain, MainFooter, NativeRequest, NativeSeed, RefreshKey, RefreshPhase,
            SelectionFence, apply_selection_key, detail_usage_request,
            reconcile_detail_usage_evidence, report_for_frame,
        };
        use crate::board_refresh::BoardRefresh;
        use crate::input::Key;
        use crate::orchestrator::Observations;
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};
        use oms_panel_core::{
            CommandOutput, Error, NativeCommand, PeerExecutor, Result, Services, Value,
        };
        use serde_json::json;
        use std::path::Path;
        use std::sync::{atomic::AtomicBool, mpsc};
        use std::time::{Duration, Instant};

        const ROOM: &str = "room-fixture";
        const MAIN: &str = "att_11111111111111111111111111111111";

        let spy = BoardSpy::new("agy-detail-usage");
        spy.provider.set("antigravity");
        let child = crate::native_children::participant_id(
            "antigravity",
            "44444444-4444-4444-4444-dddddddddddd",
        );
        let observations = spy.agy_observations(&child, TURN);
        let binding = observations.chat_binding(&child).unwrap().clone();
        let repo = binding.repo().to_path_buf();
        let pid = std::process::id();
        let birth = oms_panel_core::terminals::process(pid).unwrap().birth;
        let make_scope = |room_id: &str| BoardScope {
            key: RefreshKey::new(&repo, Some(room_id), Some(MAIN)).unwrap(),
            room: room_id.into(),
            main: Some(LiveMain {
                attempt: MAIN.into(),
                member: MAIN.into(),
                owner: "codex".into(),
                session: "$1".into(),
                window: "@1".into(),
                pane: "%1".into(),
                pid,
                birth: birth.clone(),
            }),
        };
        let current_scope = make_scope(ROOM);
        let call = json!({
            "participant": child,
            "native_child": true,
            "provider": "antigravity",
            "role": "subagent",
            "state": "working"
        });
        let report = json!({
            "collection": {"ok": true},
            "room": {
                "id": ROOM,
                "closed": false,
                "participants": [
                    {"participant": MAIN, "role": "main", "provider": "codex", "joined": true, "state": "working", "terminal": false}
                ]
            },
            "room_attempts": {
                MAIN: {
                    "attempt_id": MAIN,
                    "provider": "codex",
                    "tool": "panel-main",
                    "state": "working",
                    "terminal": false,
                    "refs": {"panel_role": "main", "panel_room_id": ROOM, "panel_room_participant": MAIN}
                }
            },
            "native_children": [call.clone()]
        });

        let req = detail_usage_request(Some(&current_scope), &report, Some(&call), &observations)
            .expect("unsampled AGY child under exact scope should request usage");
        assert_eq!(req.scope, current_scope);
        assert!(req.binding == binding);

        // Already sampled -> None.
        let mut sampled_call = call.clone();
        sampled_call["native_response_usage"] = json!({
            "scope": "latest_child_response",
            "response": {"input_tokens": 1200, "cached_input_tokens": 0, "output_tokens": 40}
        });
        assert!(
            detail_usage_request(
                Some(&current_scope),
                &report,
                Some(&sampled_call),
                &observations
            )
            .is_none()
        );

        // Non-AGY -> None.
        let mut codex_call = call.clone();
        codex_call["provider"] = json!("codex");
        assert!(
            detail_usage_request(
                Some(&current_scope),
                &report,
                Some(&codex_call),
                &observations
            )
            .is_none()
        );

        // Wrong Room -> None.
        let mut wrong_room = report.clone();
        wrong_room["room"]["id"] = json!("other-room");
        assert!(
            detail_usage_request(
                Some(&current_scope),
                &wrong_room,
                Some(&call),
                &observations
            )
            .is_none()
        );

        // Wrong Main birth -> None.
        let mut wrong_birth = current_scope.clone();
        wrong_birth.main.as_mut().unwrap().birth = "other-birth".into();
        assert!(
            detail_usage_request(Some(&wrong_birth), &report, Some(&call), &observations).is_none()
        );

        // Wrong participant -> None.
        let mut wrong_child = call.clone();
        wrong_child["participant"] = json!("native-antigravity-other");
        assert!(
            detail_usage_request(
                Some(&current_scope),
                &report,
                Some(&wrong_child),
                &observations
            )
            .is_none()
        );

        // Terminal Main membership (flag or terminal state) -> None.
        let mut terminal_flag = report.clone();
        terminal_flag["room"]["participants"][0]["terminal"] = json!(true);
        assert!(
            detail_usage_request(
                Some(&current_scope),
                &terminal_flag,
                Some(&call),
                &observations
            )
            .is_none()
        );
        let mut terminal_state = report.clone();
        terminal_state["room"]["participants"][0]["state"] = json!("done");
        assert!(
            detail_usage_request(
                Some(&current_scope),
                &terminal_state,
                Some(&call),
                &observations
            )
            .is_none()
        );

        // Targeted detail-only evidence lifecycle:
        // 1. Successful targeted response while D is open is admitted and rendered via
        //    report_for_frame without polluting raw report["native_children"].
        let mut nav = crate::navigation::Navigation::default();
        let calls = crate::render::visible(&report, &nav);
        nav.selected = calls
            .iter()
            .position(|row| row["participant"] == child)
            .unwrap();
        let mut selection = SelectionFence::None;
        apply_selection_key(&mut nav, &mut selection, &Key::Char('D'), calls.len(), 1);
        assert!(nav.detail);

        let ev = DetailUsageEvidence {
            scope: current_scope.clone(),
            binding: binding.clone(),
            response: json!({
                "model": "gemini",
                "input_tokens": 1200,
                "cached_input_tokens": 0,
                "output_tokens": 40
            }),
            observed_model: Some("gemini".into()),
            claude_version: None,
            requires_group: false,
        };
        let mut retained = None;
        reconcile_detail_usage_evidence(
            &mut retained,
            Some(ev.clone()),
            Some(&current_scope),
            &report,
            &nav,
            &observations,
            DetailUsageFrame::Observed,
            false,
        );
        assert!(retained.as_ref() == Some(&ev));
        assert!(
            report["native_children"][0]
                .get("native_response_usage")
                .is_none()
        );
        let render_detail =
            |src: &Value, n: &crate::navigation::Navigation, r: Option<&DetailUsageEvidence>| {
                let display = report_for_frame(src, n, r);
                super::frame(
                    &display,
                    n,
                    (80, 18),
                    &Default::default(),
                    false,
                    Some(&mut DockSplit::default()),
                    None,
                    None,
                )
                .lines
                .join("\n")
            };
        let detail = render_detail(&report, &nav, retained.as_ref());
        for label in [
            "Call Detail",
            "Last response",
            "1200 in",
            "40 out",
            "0 cache read",
        ] {
            assert!(detail.contains(label), "{label}: {detail}");
        }
        assert!(!detail.contains("1240"), "{detail}");

        // 2. Next ordinary ambient refresh (incoming = None, raw report still has no
        //    usage on child #4) preserves retained evidence while D stays open under
        //    the same Room/Main/pane-birth/Turn/participant.
        reconcile_detail_usage_evidence(
            &mut retained,
            None,
            Some(&current_scope),
            &report,
            &nav,
            &observations,
            DetailUsageFrame::Unchanged,
            false,
        );
        assert!(retained.as_ref() == Some(&ev));
        let detail_after_ambient = render_detail(&report, &nav, retained.as_ref());
        assert!(
            detail_after_ambient.contains("1200 in"),
            "{detail_after_ambient}"
        );
        assert!(
            !detail_after_ambient.contains("1240"),
            "{detail_after_ambient}"
        );

        let mut group = ev.clone();
        group.requires_group = true;
        let mut retained_group = Some(group.clone());
        let refresh_group = |retained: &mut Option<DetailUsageEvidence>, incoming, frame| {
            reconcile_detail_usage_evidence(
                retained,
                incoming,
                Some(&current_scope),
                &report,
                &nav,
                &observations,
                frame,
                false,
            );
        };
        refresh_group(&mut retained_group, None, DetailUsageFrame::Unchanged);
        assert!(retained_group.as_ref() == Some(&group));
        let mut renewed = group;
        renewed.response["input_tokens"] = 1300.into();
        refresh_group(
            &mut retained_group,
            Some(renewed.clone()),
            DetailUsageFrame::Observed,
        );
        assert!(retained_group.as_ref() == Some(&renewed));
        assert!(render_detail(&report, &nav, retained_group.as_ref()).contains("1300 in"));

        // The same row and Turn can survive a revoked membership or ACL change.
        refresh_group(&mut retained_group, None, DetailUsageFrame::Observed);
        assert!(retained_group.is_none());
        assert!(!render_detail(&report, &nav, retained_group.as_ref()).contains("1300 in"));
        refresh_group(
            &mut retained_group,
            Some(renewed),
            DetailUsageFrame::Observed,
        );
        assert!(render_detail(&report, &nav, retained_group.as_ref()).contains("1300 in"));

        // 3a. Closing D or moving selection before an in-flight response lands rejects
        //     incoming evidence (zero bleed).
        let mut closed_nav = nav.clone();
        closed_nav.detail = false;
        let mut bleed_check = None;
        reconcile_detail_usage_evidence(
            &mut bleed_check,
            Some(ev.clone()),
            Some(&current_scope),
            &report,
            &closed_nav,
            &observations,
            DetailUsageFrame::Observed,
            false,
        );
        assert!(bleed_check.is_none());
        let mut other_sel_nav = nav.clone();
        other_sel_nav.selected = 0; // Main row
        reconcile_detail_usage_evidence(
            &mut bleed_check,
            Some(ev.clone()),
            Some(&current_scope),
            &report,
            &other_sel_nav,
            &observations,
            DetailUsageFrame::Observed,
            false,
        );
        assert!(bleed_check.is_none());

        // 3b. Changed Turn / binding, scope change, or rejected_native drops retained
        //     evidence immediately so stale counters are redacted.
        let new_turn_obs = spy.agy_observations(&child, NEW_TURN);
        let mut stale_turn = retained.clone();
        reconcile_detail_usage_evidence(
            &mut stale_turn,
            None,
            Some(&current_scope),
            &report,
            &nav,
            &new_turn_obs,
            DetailUsageFrame::Unchanged,
            false,
        );
        assert!(stale_turn.is_none());
        let redacted = render_detail(&report, &nav, stale_turn.as_ref());
        assert!(!redacted.contains("1200 in"), "{redacted}");

        let mut on_rejected = retained.clone();
        reconcile_detail_usage_evidence(
            &mut on_rejected,
            None,
            Some(&current_scope),
            &report,
            &nav,
            &observations,
            DetailUsageFrame::Rejected,
            false,
        );
        assert!(on_rejected.is_none());

        let mut on_scope_change = retained.clone();
        reconcile_detail_usage_evidence(
            &mut on_scope_change,
            None,
            Some(&current_scope),
            &report,
            &nav,
            &observations,
            DetailUsageFrame::Observed,
            true,
        );
        assert!(on_scope_change.is_none());

        // 3c. A newer ambient native_response_usage on child #4 drops retained evidence
        //     so the newer ambient sample wins.
        let mut ambient_report = report.clone();
        ambient_report["native_children"][0]["native_response_usage"] = json!({
            "scope": "latest_child_response",
            "response": {
                "model": "gemini",
                "input_tokens": 2500,
                "cached_input_tokens": 10,
                "output_tokens": 90
            }
        });
        reconcile_detail_usage_evidence(
            &mut retained,
            None,
            Some(&current_scope),
            &ambient_report,
            &nav,
            &observations,
            DetailUsageFrame::Observed,
            false,
        );
        assert!(retained.is_none());
        let ambient_detail = render_detail(&ambient_report, &nav, retained.as_ref());
        assert!(ambient_detail.contains("2500 in"), "{ambient_detail}");
        assert!(!ambient_detail.contains("1200 in"), "{ambient_detail}");

        // InteractiveRefresh dispatches pending detail usage once
        // (immediately or on follow_up behind a busy worker), does not re-enqueue it,
        // and drops it on scope change.
        struct Collector {
            rooms: mpsc::Sender<String>,
            answer: mpsc::Receiver<Result<Value>>,
        }
        impl PeerExecutor for Collector {
            fn execute(
                &self,
                _: &oms_panel_core::services::ExecutionRequest,
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                panic!("collector must not execute processes")
            }
        }
        impl Services for Collector {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                args: &[std::ffi::OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                let value = match command {
                    NativeCommand::Events => json!([{
                        "attempt_id": MAIN, "tool": "panel-main", "provider": "claude",
                        "terminal": false, "state": "working",
                        "refs": {"panel_role": "main", "panel_room_id": ROOM}
                    }]),
                    NativeCommand::Room => {
                        self.rooms
                            .send(args[2].to_string_lossy().into())
                            .map_err(Error::new)?;
                        self.answer
                            .recv_timeout(Duration::from_secs(5))
                            .map_err(Error::new)??
                    }
                    _ => panic!("unexpected collector command {command:?}"),
                };
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&value).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }

        let (rooms, requested) = mpsc::channel();
        let (answer, responses) = mpsc::channel();
        let worker = BoardRefresh::spawn_with(move || {
            Ok(Collector {
                rooms,
                answer: responses,
            })
        })
        .unwrap();
        let mut board = InteractiveRefresh {
            worker,
            quota: Default::default(),
            scope: Some(current_scope.clone()),
            phase: RefreshPhase::Fresh,
            deferred_refresh: false,
            agy_group_renewal: None,
        };
        let make_seed = || NativeSeed {
            target: BoardTarget {
                room: Some(ROOM.into()),
                caller: None,
                managed: false,
            },
            observations: Observations::default(),
            footer: MainFooter::default(),
            agy: None,
            detail_usage: None,
            detail_usage_binding: None,
        };
        let mut seed = make_seed();
        seed.detail_usage = Some(req.clone());
        let native_request = NativeRequest {
            scope: current_scope.clone(),
            seed,
        };
        assert!(native_request.authorized_detail_usage(&report).is_some());
        let mut wrong_owner = report.clone();
        wrong_owner["room_attempts"][MAIN]["provider"] = json!("claude");
        wrong_owner["room"]["participants"][0]["provider"] = json!("claude");
        assert!(current_scope.key.matches(&oms_panel_core::state::Snapshot {
            report: wrong_owner.clone(),
            status: 0,
        }));
        assert!(
            native_request
                .authorized_detail_usage(&wrong_owner)
                .is_none()
        );
        let wait_ready = |board: &mut InteractiveRefresh| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !board.worker.ready() {
                assert!(Instant::now() < deadline, "timed out waiting for worker");
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        // 1. Immediate dispatch consumes pending once and does not re-enqueue.
        let mut pending = Some(req.clone());
        let (changed, _, _) = board.update_with_native(
            Ok(current_scope.clone()),
            true,
            false,
            Some(make_seed()),
            &mut pending,
        );
        assert!(!changed);
        assert!(pending.is_none());
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        answer.send(Err(Error::new("done"))).unwrap();
        wait_ready(&mut board);
        let _ = board.update_with_native(
            Ok(current_scope.clone()),
            false,
            true,
            Some(make_seed()),
            &mut pending,
        );
        assert!(pending.is_none());

        // 2. A queued wake races with a report whose Room and Events agree with
        // each other but disagree with the current physical Main provider.
        let _ = board.update_with_native(
            Ok(current_scope.clone()),
            true,
            false,
            Some(make_seed()),
            &mut pending,
        );
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        assert!(board.worker.busy());
        pending = Some(req.clone());
        let (changed, _, _) = board.update_with_native(
            Ok(current_scope.clone()),
            true,
            false,
            Some(make_seed()),
            &mut pending,
        );
        assert!(!changed);
        assert!(pending.is_some());
        assert!(board.deferred_refresh);
        answer.send(Ok(wrong_owner["room"].clone())).unwrap();
        wait_ready(&mut board);
        let (changed, completed, _) = board.update_with_native(
            Ok(current_scope.clone()),
            false,
            true,
            Some(make_seed()),
            &mut pending,
        );
        assert!(!changed);
        assert!(completed.is_none());
        assert_eq!(board.phase, RefreshPhase::RetryingAfterFailure);
        assert!(pending.is_none());
        let retrying = board
            .phase
            .frame_report(completed, Some(report.clone()), ROOM);
        assert_eq!(retrying["collection"]["pending"], true);
        let mut rejected = Some(ev);
        reconcile_detail_usage_evidence(
            &mut rejected,
            None,
            board.scope.as_ref(),
            &retrying,
            &nav,
            &observations,
            DetailUsageFrame::Rejected,
            changed,
        );
        assert!(rejected.is_none());
        assert!(!render_detail(&retrying, &nav, rejected.as_ref()).contains("1200 in"));
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            ROOM
        );
        answer.send(Err(Error::new("done"))).unwrap();
        wait_ready(&mut board);
        let _ = board.update_with_native(
            Ok(current_scope.clone()),
            false,
            true,
            Some(make_seed()),
            &mut pending,
        );
        assert!(pending.is_none());

        board.agy_group_renewal = Some(req.clone());
        let mut open = make_seed();
        open.detail_usage_binding = Some(binding.clone());
        let _ = board.update_with_native(
            Ok(current_scope.clone()),
            false,
            false,
            Some(open),
            &mut pending,
        );
        assert!(board.agy_group_renewal.as_ref() == Some(&req));
        let _ = board.update_with_native(
            Ok(current_scope.clone()),
            false,
            false,
            Some(make_seed()),
            &mut pending,
        );
        assert!(
            board.agy_group_renewal.is_none(),
            "closed detail must stop renewal"
        );

        // 3. Dropped on scope change.
        pending = Some(req);
        let other_scope = make_scope("room-other");
        let (changed, _, _) = board.update_with_native(
            Ok(other_scope),
            false,
            false,
            Some(make_seed()),
            &mut pending,
        );
        assert!(changed);
        assert!(pending.is_none());
        assert_eq!(
            requested.recv_timeout(Duration::from_secs(5)).unwrap(),
            "room-other"
        );
        answer.send(Err(Error::new("done"))).unwrap();
        wait_ready(&mut board);
    }

    #[test]
    fn claude_detail_usage_retains_only_the_selected_transcript_version() {
        use super::*;
        use crate::input::Key;
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        const ROOM: &str = "room-fixture";
        const MAIN: &str = "att_11111111111111111111111111111111";
        let spy = BoardSpy::new("claude-detail-usage");
        spy.provider.set("claude");
        let child = crate::native_children::participant_id("claude", "a1b2c3d4e5f60718");
        let observations = spy.claude_detail_observations(&child, TURN, 1, true);
        let binding = observations.chat_binding(&child).unwrap();
        let pid = std::process::id();
        let scope = BoardScope {
            key: RefreshKey::new(binding.repo(), Some(ROOM), Some(MAIN)).unwrap(),
            room: ROOM.into(),
            main: Some(LiveMain {
                attempt: MAIN.into(),
                member: MAIN.into(),
                owner: "codex".into(),
                session: "$1".into(),
                window: "@1".into(),
                pane: "%1".into(),
                pid,
                birth: oms_panel_core::terminals::process(pid).unwrap().birth,
            }),
        };
        let call = json!({
            "participant":child,"native_child":true,"provider":"claude","role":"subagent",
            "state":"working","title":"Selected child"
        });
        let mut report = json!({
            "collection":{"ok": true},
            "room":{"id":ROOM,"participants":[{"participant":MAIN,"role":"main","provider":"codex",
                "joined":true,"state":"working","terminal":false}]},
            "room_attempts":{MAIN:{"attempt_id":MAIN,"provider":"codex","tool":"panel-main","state":"working",
                "terminal":false,"refs":{"panel_role":"main","panel_room_id":ROOM,"panel_room_participant":MAIN}}},
            "native_children":[call,{"participant":"native-claude-other","native_child":true,
                "provider":"claude","role":"subagent","state":"working","title":"Sibling"}],
        });
        let mut nav = navigation::Navigation::default();
        let visible = render::visible(&report, &nav);
        nav.selected = visible
            .iter()
            .position(|row| row["participant"] == child)
            .unwrap();
        apply_selection_key(
            &mut nav,
            &mut SelectionFence::None,
            &Key::Char('D'),
            visible.len(),
            1,
        );
        assert!(nav.detail);
        let selected = visible.get(nav.selected);
        assert!(
            detail_usage_request(Some(&scope), &report, selected, &Observations::default())
                .is_none()
        );
        let request = detail_usage_request(Some(&scope), &report, selected, &observations)
            .expect("proved Claude locator permits one selected read");
        assert!(&request.binding == binding);

        let evidence = DetailUsageEvidence {
            scope: scope.clone(),
            binding: request.binding,
            response: json!({"input_tokens":7,"output_tokens":2,"cache_creation_input_tokens":0,
                "cache_read_input_tokens":0,"total_tokens":9}),
            observed_model: Some("claude-opus-5-5".into()),
            claude_version: observations.claude_version(&child),
            requires_group: false,
        };
        let reconcile = |retained: &mut Option<DetailUsageEvidence>,
                         incoming,
                         report: &Value,
                         nav: &navigation::Navigation,
                         obs: &Observations| {
            reconcile_detail_usage_evidence(
                retained,
                incoming,
                Some(&scope),
                report,
                nav,
                obs,
                DetailUsageFrame::Observed,
                false,
            );
        };
        let render = |retained: Option<&DetailUsageEvidence>| {
            let frame_report = report_for_frame(&report, &nav, retained);
            render::frame(&frame_report, &nav, 45, 25, &Default::default()).join("\n")
        };
        let mut retained = None;
        reconcile(
            &mut retained,
            Some(evidence.clone()),
            &report,
            &nav,
            &observations,
        );
        for _ in 0..3 {
            reconcile(&mut retained, None, &report, &nav, &observations);
            assert!(
                retained.is_some(),
                "same authoritative version preserves selected evidence"
            );
        }
        let shown = render(retained.as_ref());
        assert!(
            shown.contains("Last response") && shown.contains("0 cache read"),
            "{shown}"
        );
        assert!(render(None).contains("unreported"));
        assert!(
            report["native_children"][0]
                .get("native_response_usage")
                .is_none()
        );

        let later = spy.claude_detail_observations(&child, TURN, 2, true);
        let new_turn = spy.claude_detail_observations(&child, NEW_TURN, 1, true);
        let unchecked = spy.claude_detail_observations(&child, TURN, 1, false);
        assert_eq!(
            observations.claude_version(&child),
            unchecked.claude_version(&child)
        );
        for observation in [&later, &new_turn, &unchecked, &Observations::default()] {
            let mut retained = Some(evidence.clone());
            reconcile(&mut retained, None, &report, &nav, observation);
            assert!(retained.is_none());
        }
        let mut sibling = nav.clone();
        sibling.selected = visible
            .iter()
            .position(|row| row["participant"] == "native-claude-other")
            .unwrap();
        reconcile(&mut retained, None, &report, &sibling, &observations);
        assert!(retained.is_none());

        let duplicate = report["native_children"][0].clone();
        report["native_children"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(
            report_for_frame(&report, &nav, Some(&evidence))["native_children"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row.get("native_response_usage").is_none())
        );
    }

    #[test]
    fn refresh_fences_call_leaving_attention_filter_and_navigation_clears_fence() {
        let before = serde_json::json!({"room":{"participants":[
            {"participant":"A", "attempt":"1", "state":"blocked"},
            {"participant":"B", "attempt":"2", "state":"blocked"}
        ]}});
        let mut after = before.clone();
        after["room"]["participants"][0]["state"] = serde_json::json!("done");
        let mut nav = crate::navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let old = crate::render::visible(&before, &nav);
        let new = crate::render::visible(&after, &nav);
        assert_eq!(old.len(), 2);
        assert_eq!(new.len(), 1);
        let mut changed = super::SelectionFence::None;
        super::reconcile_selection(&old, &new, &mut nav, &mut changed);
        assert!(changed == super::SelectionFence::Changed);
        super::apply_selection_key(&mut nav, &mut changed, &crate::input::Key::Down, 1, 1);
        assert_eq!(
            super::apply_selection_key(&mut nav, &mut changed, &crate::input::Key::Enter, 1, 1),
            crate::navigation::Action::Open
        );
    }
    #[test]
    fn tty_draw_is_one_synchronized_overwrite_without_blanking() {
        let mut out = Counting(vec![], 0);
        let mut previous = vec![];
        draw(
            &mut out,
            &rows(&["a", "b", "c"]),
            true,
            3,
            &mut previous,
            false,
        )
        .unwrap();
        assert_eq!(out.1, 1, "one write per frame");
        let first = String::from_utf8(std::mem::take(&mut out.0)).unwrap();
        assert!(first.starts_with("\x1b[?2026h") && first.ends_with("\x1b[?2026l"));
        assert!(!first.contains("\x1b[2J") && !first.contains("\x1b[H"));
        assert!(
            !first.contains("\x1b[4;1H\x1b[J"),
            "full-height frame must retain its last row"
        );
        // Only the changed row is rewritten; shrinking clears the tail.
        draw(&mut out, &rows(&["a", "x"]), true, 3, &mut previous, false).unwrap();
        let second = String::from_utf8(std::mem::take(&mut out.0)).unwrap();
        assert!(second.contains("\x1b[2;1H\x1b[2Kx"));
        assert!(!second.contains("\x1b[1;1H"));
        assert!(second.contains("\x1b[3;1H\x1b[J"));
        // An unchanged frame writes nothing; resize forces a full overwrite.
        let writes = out.1;
        draw(&mut out, &rows(&["a", "x"]), true, 3, &mut previous, false).unwrap();
        assert_eq!(out.1, writes);
        draw(&mut out, &rows(&["a", "x"]), true, 3, &mut previous, true).unwrap();
        assert!(String::from_utf8_lossy(&out.0).contains("\x1b[1;1H"));
        assert!(!String::from_utf8_lossy(&out.0).contains("\x1b[2J"));
        out.0.clear();
        draw(&mut out, &rows(&["a"]), true, 1, &mut previous, false).unwrap();
        let shrunk = String::from_utf8(std::mem::take(&mut out.0)).unwrap();
        assert!(!shrunk.contains("\x1b[2;1H\x1b[J"));
    }

    #[test]
    fn agy_board_click_and_enter_open_only_the_painted_native_chat() {
        use super::*;
        use crate::orchestrator::NativeLocator;
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        let native = crate::native_children::participant_id(
            "antigravity",
            "22222222-2222-2222-2222-bbbbbbbbbbbb",
        );
        let report = serde_json::json!({
            "collection": {"ok":true},
            "provider_sessions": ["antigravity"],
            "room": {"participants":[{"participant":"call-worker","role":"worker","state":"working","joined":true}]},
            "native_children": [{
                "participant":native,"native_child":true,"provider":"antigravity","role":"subagent",
                "title":"Evidence review","state":"working","attempt":"native-generation","native_answer":"parent summary text"
            }]
        });
        let spy = BoardSpy::new("agy-routing");
        spy.provider.set("antigravity");
        let session = commands::Session::new();
        let observations = spy.agy_observations(&native, TURN);
        let mut nav = navigation::Navigation {
            view: crate::layout::View::Detail,
            ..Default::default()
        };
        let mut fence = SelectionFence::None;
        let mut chat = None;
        let calls = render::visible(&report, &nav);
        let native_index = calls.iter().position(native_chat_child).unwrap();
        let board = frame(
            &report,
            &nav,
            (160, 45),
            &Default::default(),
            false,
            Some(&mut DockSplit::default()),
            None,
            None,
        );
        let (row, hit) = board
            .worker_rows
            .iter()
            .find(|(_, hit)| hit.0 == native)
            .unwrap();
        let click = input::Key::Click(1, u16::try_from(row + 1).unwrap());
        let sample = ["ghp", "_1234567890abcdefghijklmnopqrstuvwxyz"].concat();
        let open = |call: &Value| {
            chat_dock(call, &observations, |binding| {
                spy.panel().open_chat_checked(
                    binding,
                    None,
                    session.attempt_cache(),
                    |_, terminal, locator| {
                        assert!(terminal.is_empty());
                        assert!(matches!(locator, NativeLocator::Antigravity(_)));
                        crate::native_children::chat_tail(&format!("child reply\n{sample}"))
                    },
                    |_, _| Some(TURN.into()),
                )
            })
        };
        assert!(route_chat_open(
            &click,
            Some(hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            open,
        ));
        assert_eq!(nav.selected, native_index);
        let shown = chat.as_ref().unwrap().history.clone();
        assert!(
            shown.contains("child reply") && !shown.contains(&sample),
            "{shown:?}"
        );
        assert!(chat.as_ref().unwrap().chat.is_some());
        assert!(!shown.contains("parent summary text") && !shown.contains(super::PARENT_SUMMARY));
        assert!(!refresh_chat(&mut chat, &observations, false));
        assert!(route_chat_open(
            &input::Key::Enter,
            None,
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            open,
        ));
        assert!(chat.is_some());
        let rebound = spy.agy_observations(&native, NEW_TURN);
        assert!(refresh_chat(&mut chat, &rebound, false));
        assert!(chat.is_none());
        assert_eq!(spy.capture_count.get(), 0);

        // Unbound AGY row shows parent-delivered summary, labelled, never bound as chat.
        let unbound = |call: &Value, seen: &crate::orchestrator::Observations| {
            chat_dock(call, seen, |_| panic!("read without binding")).or_else(|| {
                unbound_summary(call, seen, || {
                    worker_dock(Path::new("."), call, &spy, &AtomicBool::new(false)).ok()
                })
            })
        };
        for key in [click.clone(), input::Key::Enter] {
            chat = None;
            nav.notice = None;
            assert!(route_chat_open(
                &key,
                Some(hit),
                &calls,
                &mut nav,
                &mut fence,
                &mut chat,
                |call| unbound(call, &Default::default()),
            ));
            let dock = chat.as_ref().unwrap();
            assert!(dock.chat.is_none() && nav.notice.is_none());
            assert!(dock.history.starts_with(super::PARENT_SUMMARY));
            assert!(dock.history.ends_with("parent summary text"));
            assert!(dock.title.contains("parent summary"));
        }

        // Bound AGY child whose read fails fails closed without falling back to parent summary.
        chat = None;
        assert!(route_chat_open(
            &click,
            Some(hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            |call| chat_dock(call, &observations, |_| None).or_else(|| {
                unbound_summary(call, &observations, || panic!("summary for a bound child"))
            }),
        ));
        assert!(chat.is_none());
        assert_eq!(nav.notice.as_deref(), Some("Transcript unavailable"));

        // Bound AGY child Unavailable via open_agy_chat_dock never falls back even with native_answer;
        // Unbound AGY row with empty native_answer returns None ("Transcript unavailable") and worker_dock never adds PARENT_SUMMARY.
        let binding = observations.chat_binding(&native).unwrap();
        let open_agy = |call: &Value, seen: &crate::orchestrator::Observations| {
            open_agy_chat_dock(
                binding.repo(),
                call,
                seen,
                &spy,
                &AtomicBool::new(false),
                |b| Some(spy.panel().open_agy_chat(b, None, session.attempt_cache())),
            )
        };
        let before_captures = spy.capture_count.get();
        spy.turn.set(NEW_TURN);
        chat = None;
        nav.notice = None;
        assert!(route_chat_open(
            &click,
            Some(hit),
            &calls,
            &mut nav,
            &mut fence,
            &mut chat,
            |call| open_agy(call, &observations),
        ));
        spy.turn.set(TURN);
        assert_eq!(spy.capture_count.get(), before_captures + 1);
        assert!(chat.is_none());
        assert_eq!(nav.notice.as_deref(), Some("Transcript unavailable"));

        let mut silent_call = calls[native_index].clone();
        silent_call["native_answer"] = serde_json::json!("");
        let silent_dock =
            worker_dock(Path::new("."), &silent_call, &spy, &AtomicBool::new(false)).unwrap();
        assert!(!silent_dock.title.contains("parent summary"));
        assert!(!silent_dock.history.contains(super::PARENT_SUMMARY));
        assert!(open_agy(&silent_call, &observations).is_none());
        assert!(
            open_agy_chat_dock(
                binding.repo(),
                &silent_call,
                &Default::default(),
                &spy,
                &AtomicBool::new(false),
                |_| panic!("unbound read"),
            )
            .is_none()
        );

        let dock = ChatDock {
            title: "t".into(),
            history: "h".into(),
            back: 0,
            chat: Some((
                native.clone(),
                observations.chat_binding(&native).unwrap().clone(),
            )),
            agy_locator: None,
            main: None,
        };
        assert!(private_chat_hidden(
            &serde_json::json!({"collection": {"pending": true}}),
            &dock
        ));
        assert!(!private_chat_hidden(&report, &dock));
    }

    #[test]
    fn agy_board_discards_transcript_after_live_proof_changes() {
        use crate::orchestrator::codex_chat_fixture::{BoardSpy, NEW_TURN, TURN};

        let native = "native-antigravity-child";
        let session = super::commands::Session::new();
        let open = |spy: &BoardSpy, binding, turn: &'static str| {
            spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| Some("transcript".into()),
                |_, _| Some(turn.into()),
            )
        };

        let codex = BoardSpy::new("agy-vs-codex");
        let observed = codex.observations("native-codex-child", TURN);
        let codex_binding = observed.chat_binding("native-codex-child").unwrap();
        assert!(
            codex
                .panel()
                .open_chat_with(codex_binding, None, session.attempt_cache(), |_, _, _| {
                    Some("t".into())
                })
                .is_some()
        );
        let spy = BoardSpy::new("agy-live");
        spy.provider.set("antigravity");
        let observations = spy.agy_observations(native, TURN);
        let binding = observations.chat_binding(native).unwrap();
        assert_eq!(open(&spy, binding, TURN).as_deref(), Some("transcript"));
        assert_eq!(
            (spy.list_count.get(), spy.show_count.get()),
            (codex.list_count.get(), codex.show_count.get())
        );
        assert_eq!((codex.capture_count.get(), spy.capture_count.get()), (2, 0));

        assert!(open(&spy, binding, NEW_TURN).is_none());
        let refused = |spy: &BoardSpy| {
            spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| panic!("unproven pane reached the transcript"),
                |_, _| panic!("unproven pane reached the Turn"),
            )
        };
        spy.lead.set("%4");
        assert!(refused(&spy).is_none());
        spy.lead.set("%3");
        spy.lead_pid.set(Some(1));
        assert!(refused(&spy).is_none());
        spy.lead_pid.set(None);
        spy.room_closed.set(true);
        assert!(refused(&spy).is_none());
        spy.room_closed.set(false);
        spy.main_terminal.set(true);
        assert!(refused(&spy).is_none());
        spy.main_terminal.set(false);
        spy.docked.set(true);
        assert!(refused(&spy).is_none());
        spy.docked.set(false);
        spy.provider.set("codex");
        assert!(refused(&spy).is_none());
        spy.provider.set("antigravity");
        assert!(open(&spy, binding, TURN).is_some());

        let moved = spy.panel().open_chat_checked(
            binding,
            None,
            session.attempt_cache(),
            |_, _, _| {
                spy.lead.set("%4");
                Some("moved".into())
            },
            |_, _| Some(TURN.into()),
        );
        assert!(moved.is_none());
        spy.lead.set("%3");
        for lose in [
            &(|| spy.room_closed.set(true)) as &dyn Fn(),
            &(|| spy.main_terminal.set(true)),
        ] {
            let lost = spy.panel().open_chat_checked(
                binding,
                None,
                session.attempt_cache(),
                |_, _, _| {
                    lose();
                    Some("lost".into())
                },
                |_, _| Some(TURN.into()),
            );
            assert!(lost.is_none());
            spy.room_closed.set(false);
            spy.main_terminal.set(false);
        }

        // settle_live_chat_dock_with never touches non-AGY docks (0 capture calls), and closes
        // an open AGY ChatDock on !pending when recheck_open_agy_chat returns false (missing/unproved locator).
        use super::{ChatDock, settle_live_chat_dock_with};
        use oms_panel_core::{PeerExecutor, services::ExecutionRequest};
        use std::path::Path;
        use std::sync::atomic::AtomicBool;

        let before_captures = spy.capture_count.get();
        let codex_report = serde_json::json!({"collection":{"ok":true},"native_dock":null});
        let mut codex_dock = Some(ChatDock {
            title: "codex".into(),
            history: "h".into(),
            back: 0,
            chat: Some(("native-codex-child".into(), codex_binding.clone())),
            agy_locator: None,
            main: None,
        });
        assert!(!settle_live_chat_dock_with(
            &mut codex_dock,
            &observed,
            &codex_report,
            |_, _| panic!("codex dock triggered agy recheck"),
        ));
        assert!(codex_dock.is_some());
        assert_eq!(spy.capture_count.get(), before_captures);

        let agy_child = crate::native_children::participant_id(
            "antigravity",
            "22222222-2222-2222-2222-bbbbbbbbbbbb",
        );
        let agy_child_obs = spy.agy_observations(&agy_child, TURN);
        let agy_child_binding = agy_child_obs.chat_binding(&agy_child).unwrap().clone();
        let dummy_locator = crate::agy_child_chat::AgyChildChatLocator {
            pid: std::process::id(),
            birth: spy.agy_locator(&agy_child, TURN).birth,
            app_root: std::path::PathBuf::from("/nonexistent"),
            record: crate::agy_child_records::AgyChildLocator {
                lead_id: "aaaaaaaa-1111-2222-3333-444444444444".into(),
                turn: TURN.into(),
                child_id: "22222222-2222-2222-2222-bbbbbbbbbbbb".into(),
                spawn_step: 2,
                result_step: 3,
                boundary: "b".into(),
            },
            lock: (0, 0),
            lead: (0, 0),
            lead_min_size: 0,
            child: (0, 0),
            child_min_size: 0,
        };
        let mut agy_dock = Some(ChatDock {
            title: "agy".into(),
            history: "h".into(),
            back: 0,
            chat: Some((agy_child.clone(), agy_child_binding)),
            agy_locator: Some(dummy_locator),
            main: None,
        });
        assert!(!settle_live_chat_dock_with(
            &mut agy_dock,
            &agy_child_obs,
            &serde_json::json!({"collection":{"pending":true},"native_dock":null}),
            |_, _| panic!("pending frame triggered agy recheck"),
        ));
        assert!(agy_dock.is_some());
        assert!(settle_live_chat_dock_with(
            &mut agy_dock,
            &agy_child_obs,
            &codex_report,
            |b, loc| spy
                .panel()
                .recheck_open_agy_chat(b, loc, None, session.attempt_cache()),
        ));
        assert!(agy_dock.is_none());
        assert_eq!(spy.capture_count.get(), before_captures + 1);

        for bad_args in [
            vec![
                "tmux",
                "list-panes",
                "-s",
                "-t",
                "%0",
                "-F",
                crate::orchestrator::PANE_FORMAT,
            ],
            vec!["tmux", "send-keys", "-t", "%3", "Enter"],
        ] {
            let req = ExecutionRequest::new(super::commands::strings(&bad_args), Path::new("."));
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = spy.execute(&req, &AtomicBool::new(false));
                }))
                .is_err()
            );
        }
    }

    #[cfg(target_os = "linux")]
    const WATCH_PTY: &str = "OMS_TEST_AGY_WATCH_PTY";

    #[cfg(target_os = "linux")]
    static PROVIDER_COUNTERS: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::new([0; 8]);

    #[cfg(target_os = "linux")]
    fn agy_watch_counters() -> [usize; 8] {
        use crate::agy_child_chat as agy;
        let (custody, scans, strict_reads) = agy::test_counters();
        let (legacy_reads, strict_check) = agy::agy_child_read_counts();
        let (leg_custody, leg_parent, leg_cand, leg_child) = agy::legacy_agy_read_index();
        assert_eq!(strict_reads, strict_check);
        assert_eq!(legacy_reads, leg_child);
        assert_eq!(agy::total_agy_child_reads(), legacy_reads + strict_reads);
        [
            custody,
            scans,
            strict_reads,
            legacy_reads,
            leg_custody,
            leg_parent,
            leg_cand,
            leg_child,
        ]
    }

    #[cfg(target_os = "linux")]
    pub(super) fn record_provider_counters() {
        if std::env::var_os(WATCH_PTY).is_some() {
            *PROVIDER_COUNTERS.lock().unwrap() = agy_watch_counters();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn agy_watch_real_private_fake_tmux_click_enter_escape_and_fallback() {
        private_native_watch_fixture(
            "watch::tests::agy_watch_real_private_fake_tmux_click_enter_escape_and_fallback",
            None,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_watch_paints_bound_child_only_rollout() {
        private_native_watch_fixture(
            "watch::tests::codex_watch_paints_bound_child_only_rollout",
            Some(CodexWatchStimulus::Complete),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_watch_retries_after_partial_bound_child_header() {
        private_native_watch_fixture(
            "watch::tests::codex_watch_retries_after_partial_bound_child_header",
            Some(CodexWatchStimulus::Partial),
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn codex_watch_paints_verified_recent_creation_in_default_overview() {
        private_native_watch_fixture(
            "watch::tests::codex_watch_paints_verified_recent_creation_in_default_overview",
            Some(CodexWatchStimulus::Overview),
        );
    }

    #[cfg(target_os = "linux")]
    enum CodexWatchStimulus {
        Complete,
        Partial,
        Overview,
    }

    #[cfg(target_os = "linux")]
    fn private_native_watch_fixture(test_name: &str, stimulus: Option<CodexWatchStimulus>) {
        use super::Options;
        use crate::{commands, render};
        use oms_panel_core::{CommandOutput, NativeCommand, Services, SubcommandServices};
        use serde_json::{Value, json};
        use std::io::{BufRead, BufReader, Read, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::process::{Command, Stdio};
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        const SEED_ENV: &str = "OMS_TEST_AGY_WATCH_SEED";
        const REPLY_ENV: &str = "OMS_TEST_WATCH_REPLY";
        const WORKER: &str = "wake-worker";
        const OVERVIEW_HISTORY: [&str; 3] =
            ["Codex archived A", "Codex archived B", "Codex archived C"];
        const OVERVIEW_CHILD: &str = "Codex new child fixture";
        const OVERVIEW_PARTIAL_MODEL: &str = "fixture/partial-frame:v2";
        let codex = stimulus.is_some();
        let overview = matches!(stimulus, Some(CodexWatchStimulus::Overview));
        let complete_overview_header = std::cell::Cell::new(false);
        let timing = matches!(stimulus, Some(CodexWatchStimulus::Complete))
            .then(|| std::env::var("OMS_TEST_CODEX_ROSTER_TIMING").ok())
            .flatten();
        let (trials, socket_fallback): (usize, bool) = match timing.as_deref() {
            Some("paired") => (20, true),
            Some("fast") => (1, false),
            Some("socket") => (1, true),
            None => (1, true),
            _ => panic!("invalid Codex fixture timing mode"),
        };

        let reply = std::env::var_os(REPLY_ENV).is_some();
        if reply || std::env::var_os(SEED_ENV).is_some() {
            let repo = PathBuf::from(std::env::var_os("OMS_TEST_REPO").unwrap());
            let svc = SubcommandServices::sibling().unwrap();
            let stop = AtomicBool::new(false);
            let run_cmd = |kind: NativeCommand, args: &[&str]| -> CommandOutput {
                let out = svc
                    .command(kind, &repo, &commands::strings(args), &stop)
                    .unwrap();
                assert!(
                    out.success().is_ok(),
                    "{kind:?} {args:?} failed code={} stdout={} stderr={}",
                    out.code,
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                );
                out
            };
            if reply {
                let room = std::env::var("OMS_FAKE_ROOM").unwrap();
                let main = std::env::var("OMS_FAKE_ATTEMPT").unwrap();
                run_cmd(
                    NativeCommand::Room,
                    &[
                        "send",
                        "--id",
                        &room,
                        "--participant",
                        WORKER,
                        "--to",
                        &main,
                        "--kind",
                        "answer",
                        "--text",
                        "Board wake reply",
                        "--json",
                    ],
                );
                // Service calls in this fixture are in-process. Notify on completion
                // just as the front door does after a successful room command.
                for _ in 0..8 {
                    crate::wakeup::notify_wakeup(&repo);
                }
                return;
            }
            let room_id: String =
                run_cmd(NativeCommand::Room, &["new", "--topic", "PTY", "--json"])
                    .json()
                    .unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
            let attempt_out = run_cmd(
                NativeCommand::Events,
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
                    &format!("panel_room_id={room_id}"),
                    "--ref",
                    "panel_native_scheduling=unmanaged",
                    "--then",
                    "starting",
                    "--then",
                    "working",
                ],
            );
            let attempt_id = String::from_utf8(attempt_out.stdout)
                .unwrap()
                .trim()
                .to_owned();
            run_cmd(
                NativeCommand::Room,
                &[
                    "join",
                    "--id",
                    &room_id,
                    "--participant",
                    &attempt_id,
                    "--provider",
                    "codex",
                    "--role",
                    "main",
                    "--json",
                ],
            );
            run_cmd(
                NativeCommand::Room,
                &[
                    "join",
                    "--id",
                    &room_id,
                    "--participant",
                    WORKER,
                    "--provider",
                    "codex",
                    "--role",
                    "worker",
                    "--parent",
                    &attempt_id,
                    "--json",
                ],
            );
            const THREAD: &str = "panel-council-0123456789abcdef01234567";
            run_cmd(
                NativeCommand::Events,
                &[
                    "start",
                    "--provider",
                    "codex",
                    "--tool",
                    "panel-council",
                    "--ref",
                    &format!("panel_room_id={room_id}"),
                    "--ref",
                    &format!("panel_room_participant={attempt_id}"),
                    "--ref",
                    "panel_label=Council fixture title",
                    "--ref",
                    &format!("panel_thread={THREAD}"),
                ],
            );
            let threads = repo.join(".oms/threads");
            std::fs::create_dir_all(&threads).unwrap();
            std::fs::set_permissions(&threads, std::fs::Permissions::from_mode(0o700)).unwrap();
            let log = threads.join(format!("{THREAD}.jsonl"));
            std::fs::write(&log, format!(
                "{{\"thread\":\"{THREAD}\",\"role\":\"answer\",\"agent\":\"claude\",\"text\":\"COUNCIL_POSITION_ONE\"}}\n{{\"thread\":\"{THREAD}\",\"role\":\"answer\",\"agent\":\"codex\",\"text\":\"COUNCIL_POSITION_TWO\"}}\n"
            )).unwrap();
            std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).unwrap();
            let seed_out = PathBuf::from(std::env::var_os("OMS_TEST_SEED_OUT").unwrap());
            std::fs::write(&seed_out, format!("{room_id}\n{attempt_id}\n")).unwrap();
            return;
        }

        if let Some(tty) = std::env::var_os(WATCH_PTY) {
            // The watch needs its terminal as controlling terminal. A session leader that
            // has none acquires the terminal it opens.
            rustix::process::setsid().unwrap();
            let _controlling = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&tty)
                .unwrap();
            let repo = PathBuf::from(std::env::var_os("OMS_TEST_REPO").unwrap());
            let counters_file = PathBuf::from(std::env::var_os("OMS_TEST_COUNTERS").unwrap());
            let options = Options::parse(&commands::strings(&["--watch"])).unwrap();
            let services = SubcommandServices::sibling().unwrap();
            let interrupted = AtomicBool::new(false);
            if codex {
                crate::orchestrator::codex_chat_fixture::bind_codex_watch_lead(
                    &repo,
                    &std::env::var("OMS_FAKE_ROOM").unwrap(),
                    &services,
                    &interrupted,
                );
            }
            crate::agy_child_chat::reset_all_agy_counters();
            let result = super::run(&repo, &options, &services, &interrupted).unwrap();
            let foreground = agy_watch_counters();
            let observed = PROVIDER_COUNTERS.lock().unwrap();
            let counts = foreground
                .into_iter()
                .zip(*observed)
                .map(|(action, refresh)| (action + refresh).to_string())
                .collect::<Vec<_>>()
                .join(",");
            std::fs::write(&counters_file, counts).unwrap();
            assert_eq!(result.0, 0);
            return;
        }

        struct Temp(PathBuf);
        impl Temp {
            fn new(label: &str) -> Self {
                let p = std::env::temp_dir()
                    .canonicalize()
                    .unwrap()
                    .join(format!("oms-watch-pty-{label}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir(&p).unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
                Self(p)
            }
        }
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let mkdir_p = |p: &Path| {
            std::fs::create_dir_all(p).unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        let chmod_tree = |root: &Path| {
            fn walk(p: &Path) {
                let m = std::fs::symlink_metadata(p).unwrap();
                if m.file_type().is_symlink() {
                    return;
                }
                if m.is_dir() {
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
                    for e in std::fs::read_dir(p).unwrap() {
                        walk(&e.unwrap().path());
                    }
                } else {
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
                }
            }
            walk(root);
        };

        let label = match stimulus {
            None => "agy",
            Some(CodexWatchStimulus::Complete) => "codex",
            Some(CodexWatchStimulus::Partial) => "partial",
            Some(CodexWatchStimulus::Overview) => "overview",
        };
        let tmp = Temp::new(label);
        // A long checkout selects the real five-second no-socket fallback
        // in the fast smoke. The full timing pairs use the normal socket.
        let repo = tmp.0.join(if socket_fallback {
            "repo".into()
        } else {
            "r".repeat(99)
        });
        let wt = tmp.0.join("repo-wt");
        let home = tmp.0.join("home");
        let state = tmp.0.join("state");
        let bin = tmp.0.join("bin");
        let log = tmp.0.join("tmux-calls.log");
        let counters = tmp.0.join("counters.txt");
        let watch_pid = tmp.0.join("watch-pid");
        let gemini = home.join(".gemini");
        let app_root = gemini.join("antigravity-cli");
        let presence = app_root.join("presence");
        let brain = app_root.join("brain");
        for d in [
            &repo, &home, &state, &bin, &gemini, &app_root, &presence, &brain,
        ] {
            mkdir_p(d);
        }
        let codex_home = home.join(".codex");
        if codex {
            mkdir_p(&codex_home);
            let xdg = home.join(".config");
            mkdir_p(&xdg);
            let registry_dir = xdg.join("oh-my-setting");
            mkdir_p(&registry_dir);
            let registry = registry_dir.join("provider-accounts.json");
            std::fs::write(
                &registry,
                json!({"schema":1,"providers":{"codex":{"accounts":[{"id":"fixture","home":codex_home}]}}}).to_string(),
            )
            .unwrap();
            std::fs::set_permissions(registry, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let run_git = |dir: &Path, args: &[&str]| {
            let mut c = Command::new("git");
            for k in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_INDEX_FILE",
                "GIT_COMMON_DIR",
            ] {
                c.env_remove(k);
            }
            let st = c
                .current_dir(dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("HOME", &home)
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
            assert!(st.success(), "git {args:?}");
        };
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        run_git(
            &repo,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        chmod_tree(&repo);
        chmod_tree(&wt);

        // Seed real room & live Main attempt in an env_clear() subprocess (zero host/parent env leakage).
        let seed_out = tmp.0.join("seed.txt");
        let exe = std::env::current_exe().unwrap();
        let fixture_command = || {
            let mut cmd = Command::new(&exe);
            cmd.args(["--exact", test_name, "--nocapture"])
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .env("HOME", &home)
                .env("TMPDIR", &tmp.0)
                .env("XDG_STATE_HOME", &state)
                .env("OMS_TEST_REPO", &repo);
            cmd
        };
        let seed_status = fixture_command()
            .env(SEED_ENV, "1")
            .env("OMS_TEST_SEED_OUT", &seed_out)
            .status()
            .unwrap();
        assert!(
            seed_status.success(),
            "seed subprocess failed: {seed_status:?}"
        );
        let seed_raw = std::fs::read_to_string(&seed_out).unwrap();
        let (room_id, attempt_id) = seed_raw.trim().split_once('\n').unwrap();
        let room_id = room_id.to_owned();
        let attempt_id = attempt_id.to_owned();

        let lead_id = "aaaaaaaa-1111-2222-3333-444444444444";
        let child_a = "22222222-2222-2222-2222-bbbbbbbbbbbb";
        let child_b = "33333333-3333-3333-3333-cccccccccccc";
        let child_c = "44444444-4444-4444-4444-dddddddddddd";
        let turn = crate::orchestrator::codex_chat_fixture::TURN;
        let lock_path = presence.join(format!("{lead_id}.lock"));
        std::fs::write(&lock_path, b"").unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mut holder = Command::new("flock")
            .arg("-n")
            .arg("-F")
            .arg(&lock_path)
            .args([
                "bash",
                "-c",
                if codex {
                    "exec -a codex bash"
                } else {
                    "exec -a agy bash"
                },
            ])
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

        let prompt_line = format!(
            "{} Turn {turn}. You own this provider's work. Check workspace",
            crate::native_children::TASK_PREFIX
        );
        let next_turn = crate::orchestrator::codex_chat_fixture::NEW_TURN;
        let next_prompt_line = format!(
            "{} Turn {next_turn}. You own this provider's work. Next",
            crate::native_children::TASK_PREFIX
        );
        let prompt_file = tmp.0.join("fake-prompt.txt");
        let write_prompt = |line: &str| {
            let p_tmp = tmp.0.join("fake-prompt.tmp");
            std::fs::write(&p_tmp, format!("> {line}\n")).unwrap();
            std::fs::rename(&p_tmp, &prompt_file).unwrap();
        };
        write_prompt(&prompt_line);
        let preamble = "The following is a <SYSTEM_MESSAGE> not actually sent by the user. It is provided by the system as important information to pay attention to.\n\n";
        let repo_uri = format!("file://{}", repo.display());
        let wt_uri = format!("file://{}", wt.display());
        let lead_rows = vec![
            json!({"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":format!("<USER_REQUEST>\n{prompt_line}\n</USER_REQUEST>")}),
            json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"spawning c","tool_calls":[{"id":"tc","name":"invoke_subagent","args":{"Prompt":"silent legacy","Role":"Silent Legacy","TypeName":"general"}}]}),
            json!({"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T17:59:59Z","content":format!("Created the following subagent:\n{:#}", json!({"conversationId":child_c,"workspaceUris":[wt_uri]}))}),
            json!({"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"spawning b","tool_calls":[{"id":"tb","name":"invoke_subagent","args":{"Prompt":"review sibling","Role":"Sibling Reviewer","TypeName":"general"}}]}),
            json!({"step_index":4,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:00Z","content":format!("Created the following subagent:\n{:#}", json!({"conversationId":child_b,"workspaceUris":[wt_uri]}))}),
            json!({"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"spawning a","tool_calls":[{"id":"ta","name":"invoke_subagent","args":{"Prompt":"inspect crate","Role":"Bound Researcher","TypeName":"general"}}]}),
            json!({"step_index":6,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:00:01Z","content":format!("Created the following subagent:\n{:#}", json!({"conversationId":child_a,"workspaceUris":[repo_uri]}))}),
            json!({"step_index":7,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-11-18T18:01:00Z","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:01:00Z sender={child_a} priority=HIGH content=PARENT_DELIVERED_SUMMARY_A\n</SYSTEM_MESSAGE>")}),
            json!({"step_index":8,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":"2026-11-18T18:01:01Z","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:01:01Z sender={child_b} priority=HIGH content=PARENT_DELIVERED_SUMMARY_B\n</SYSTEM_MESSAGE>")}),
        ];
        let child_a_rows = vec![
            json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:00:01Z sender={lead_id} content=inspect crate\n</SYSTEM_MESSAGE>")}),
            json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"OWN_CHILD_DIRECT_REPLY","thinking":"HIDDEN_THINKING"}),
        ];
        let child_b_rows = vec![
            json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T18:00:00Z sender={lead_id} content=review sibling\n</SYSTEM_MESSAGE>")}),
            json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"SIBLING_NEVER_READ"}),
        ];
        let child_c_rows = vec![
            json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","content":format!("{preamble}<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-18T17:59:59Z sender={lead_id} content=silent legacy\n</SYSTEM_MESSAGE>")}),
            json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"UNPROVED_SILENT_LEGACY_REPLY"}),
        ];
        let write_jsonl = |id: &str, name: &str, rows: &[Value]| {
            let id_dir = brain.join(id);
            let sys_dir = id_dir.join(".system_generated");
            let logs = sys_dir.join("logs");
            for d in [&id_dir, &sys_dir, &logs] {
                mkdir_p(d);
            }
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
        write_jsonl(child_a, "transcript.jsonl", &child_a_rows);
        write_jsonl(child_a, "transcript_full.jsonl", &child_a_rows);
        write_jsonl(child_b, "transcript.jsonl", &child_b_rows);
        write_jsonl(child_b, "transcript_full.jsonl", &child_b_rows);
        write_jsonl(child_c, "transcript.jsonl", &child_c_rows);
        write_jsonl(child_c, "transcript_full.jsonl", &child_c_rows);

        let stall_capture = tmp.0.join("capture-stall");
        let capture_entered = tmp.0.join("capture-entered");
        let model = tmp.0.join("provider-model");
        let lead_window = tmp.0.join("lead-window");
        let quota_stage = tmp.0.join("quota-stage");
        let set_model = |label: &str| std::fs::write(&model, label).unwrap();
        set_model("Fixture initial");
        std::fs::write(&lead_window, "@4").unwrap();

        // Private fake tmux script logging all calls; PATH holds ONLY <tmp>/bin (with real git symlinked).
        let git_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("git"))
            .find(|p| p.is_file())
            .unwrap();
        std::os::unix::fs::symlink(&git_bin, bin.join("git")).unwrap();
        let fake_tmux = bin.join("tmux");
        let fake_tmux_src = r##"#!/bin/sh
# Private fake tmux for the watch fixture. The fixture PATH holds no tools, so
# this uses shell builtins and absolute /bin paths only.
T=$(printf '\t')
S=$(printf '\037')
join() { r=; for a in "$@"; do r="$r$S$a"; done; }
join "$@"; call=$r
is() { join "$@"; [ "$call" = "$r" ]; }
log=${OMS_FAKE_TMUX_LOG:-}
[ -z "$log" ] || printf '%s\n' "$*" >> "$log"
stall=${OMS_FAKE_STALL_CAPTURE:-}
stall_ticket() {
  [ -n "$stall" ] && [ -e "$stall" ] && /bin/cat "$stall"
}
hold_capture() {
  ticket=$1
  printf '%s\n%s' "$ticket" "$$" > "${OMS_FAKE_CAPTURE_ENTERED?}"
  while [ -e "$stall" ] && [ "$(/bin/cat "$stall" 2>/dev/null)" = "$ticket" ]; do /bin/sleep 0.01; done
}
quota=${OMS_FAKE_QUOTA_STAGE:-}
stage=
if [ -n "$quota" ] && [ -e "$quota" ]; then IFS= read -r stage < "$quota" || :; fi
advance() { printf '%s' "$1" > "$quota"; }
PANES="#{session_id}$T#{window_id}$T#{pane_id}$T#{pane_pid}$T#{pane_dead}$T#{@oms_panel_main_watch}$T#{@oms_panel_main_attempt}$T#{@oms_panel_native_pane}$T#{@oms_panel_owner}$T#{@oms_panel_room_participant}$T#{@oms_panel_room}$T#{@oms_panel_repo}$T.$T#{?#{m/r:[[:cntrl:]],#{pane_start_path}},,#{pane_start_path}}"
SESSION="#{session_id}$T#{window_id}$T#{pane_id}$T#{pane_pid}$T#{pane_dead}$T#{window_active}$T#{@oms_panel_native_pane}$T#{@oms_panel_main_attempt}$T#{@oms_panel_owner}$T#{@oms_panel_room_participant}$T#{@oms_panel_room}$T#{@oms_provider_session}$T#{@oms_provider_parent}$T#{pane_active}$T.#{@oms_panel_main_account_ready}_#{@oms_codex_chat_bound}_#{@oms_claude_quota_bound}"
SERVER="#{session_id}$T#{pid}$T#{socket_path}$T."
WATCH="#{session_id}$T#{window_id}$T#{pane_pid}$T#{pane_dead}$T#{window_active}$T#{pane_in_mode}$T#{@oms_panel_main_watch}$T#{@oms_panel_main_attempt}$T#{@oms_panel_room}$T#{@oms_panel_repo}"
if is list-panes -t %2 -F "$PANES"; then
  repo=${OMS_TEST_REPO?} room=${OMS_FAKE_ROOM?} attempt=${OMS_FAKE_ATTEMPT?} execution=${OMS_TEST_EXECUTION?}
  if [ -n "$stall" ] && [ -e "$stall.sampled" ] && [ "$(stall_ticket)" = native-after-sample ]; then
    hold_capture native-after-sample
  fi
  printf '$1\t@1\t%%2\t%s\t0\t1\t%s\t%%1\tcodex\t%s\t%s\t%s\t.\n' "$PPID" "$attempt" "$attempt" "$room" "$repo"
  printf '$1\t@1\t%%1\t%s\t0\t0\t%s\t%%1\tcodex\t%s\t%s\t%s\t.\t%s\n' "$PPID" "$attempt" "$attempt" "$room" "$repo" "$execution"
elif is list-panes -s -t %2 -F "$SESSION"; then
  room=${OMS_FAKE_ROOM?} attempt=${OMS_FAKE_ATTEMPT?} lead_pid=${OMS_FAKE_LEAD_PID?}
  printf '$1\t@1\t%%1\t%s\t0\t1\t%%1\t%s\tcodex\t%s\t%s\t\t\t1\t.__\n' "$PPID" "$attempt" "$attempt" "$room"
  printf '$1\t@1\t%%2\t%s\t0\t1\t%%1\t%s\tcodex\t%s\t%s\t\t\t0\t.__\n' "$PPID" "$attempt" "$attempt" "$room"
  window=@4
  if [ -n "${OMS_FAKE_LEAD_WINDOW+x}" ]; then IFS= read -r window < "$OMS_FAKE_LEAD_WINDOW" || :; fi
  if [ "$window" != absent ]; then
    if [ -n "${OMS_TEST_CODEX_LEAD:-}" ]; then
      printf '$1\t%s\t%%3\t%s\t0\t0\t\t\t\t\t%s\tcodex\t%s\t0\t._1_\n' "$window" "$lead_pid" "$room" "$attempt"
    else
      printf '$1\t%s\t%%4\t%s\t0\t0\t\t\t\t\t%s\tantigravity\t%s\t0\t.__\n' "$window" "$lead_pid" "$room" "$attempt"
    fi
  fi
elif is display-message -p -t '$1' "$SERVER" ';' show-options -v -t '$1' @oms_panel_repo; then
  repo=${OMS_TEST_REPO?}
  printf '$1\t%s\tfake-socket\t.\n%s\n' "$PPID" "$repo"
elif is display-message -p -t %2 '#{pane_width} #{pane_height}'; then
  printf '160 45\n'
elif is display-message -p -t %2 "$WATCH"; then
  repo=${OMS_TEST_REPO?} room=${OMS_FAKE_ROOM?} attempt=${OMS_FAKE_ATTEMPT?}
  printf '$1\t@1\t%s\t0\t1\t0\t1\t%s\t%s\t%s\n' "$PPID" "$attempt" "$room" "$repo"
elif [ -n "$stage" ] && is display-message -p -t %4 '#{pane_in_mode}|#{cursor_x}|#{cursor_y}'; then
  printf '0|2|0\n'
elif [ -n "$stage" ] && { is capture-pane -p -t %4 -S 0 -E 0 || is capture-pane -p -e -S 0 -t %4; }; then
  if [ "$stage" = draft ]; then printf '> /usage\n'; else printf '>\n'; fi
elif [ "$stage" = armed ] && [ $# -eq 5 ] && [ "$1" = set-buffer ] && [ "$2" = -b ] && [ "$4" = -- ] && [ "$5" = /usage ]; then
  :
elif [ "$stage" = armed ] && [ $# -eq 7 ] && [ "$1" = paste-buffer ] && [ "$2" = -p ] && [ "$3" = -d ] && [ "$4" = -b ] && [ "$6" = -t ] && [ "$7" = %4 ]; then
  advance draft
elif [ "$stage" = draft ] && is send-keys -t %4 Enter; then
  advance dialog
elif [ "$stage" = confirm ] && is send-keys -t %4 Escape; then
  advance closed
elif { [ "$stage" = dialog ] || [ "$stage" = confirm ]; } && is capture-pane -p -t %4; then
  if [ "$stage" = dialog ]; then
    advance confirm
  else
    stall=${OMS_FAKE_STALL_CAPTURE?}
    printf 'quota\n%s' "$$" > "${OMS_FAKE_CAPTURE_ENTERED?}"
    while [ -e "$stall" ]; do /bin/sleep 0.01; done
  fi
  printf '%s\n' '└ Models & Quota' 'GEMINI MODELS' 'Weekly Limit Remaining' '[███] 94%' 'Five Hour Limit Remaining' '[███] 86%' 'CLAUDE AND GPT MODELS' 'Weekly Limit Remaining' '[███] 100%'
elif is capture-pane -p -t %1; then
  printf '› \n\n  GPT-6.2-Sol high · Context 9%% left · weekly 13%% left · main · ~/fixture · Main [default]\n← for agents · ? for shortcuts\n'
elif [ -n "${OMS_TEST_CODEX_LEAD:-}" ] && { is capture-pane -p -t %3 || is capture-pane -p -J -S -100 -t %3; }; then
  /bin/cat "${OMS_FAKE_PROMPT_FILE?}"
elif is capture-pane -p -t %4 || is capture-pane -p -J -S -100 -t %4; then
  if [ -n "$stall" ] && [ -e "$stall" ]; then
    ticket=$(stall_ticket)
    if [ "$ticket" = native-after-sample ]; then
      if is capture-pane -p -J -S -100 -t %4; then : > "$stall.sampled"; fi
    elif is capture-pane -p -t %4; then
      hold_capture "$ticket"
    fi
  fi
  /bin/cat "${OMS_FAKE_PROMPT_FILE?}"
  if [ -n "${OMS_FAKE_PROVIDER_MODEL+x}" ]; then
    IFS= read -r model < "$OMS_FAKE_PROVIDER_MODEL" || :
    printf '%s (Antigravity ) | ctx 32%% | main\n' "$model"
  fi
else
  [ -z "$log" ] || printf 'UNEXPECTED: %s\n' "$*" >> "$log"
  printf 'unexpected fake tmux call: %s\n' "$*" >&2
  exit 1
fi
"##;
        std::fs::write(&fake_tmux, fake_tmux_src).unwrap();
        std::fs::set_permissions(&fake_tmux, std::fs::Permissions::from_mode(0o700)).unwrap();

        let count_captures = || -> usize {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("capture-pane ") && l.ends_with("-t %4"))
                .count()
        };
        let apply_frames = |stream: &mut String, screen: &mut [String]| -> bool {
            let mut updated = false;
            while let Some(start) = stream.find("\x1b[?2026h") {
                let after = &stream[start + 8..];
                let Some(end) = after.find("\x1b[?2026l") else {
                    break;
                };
                let block = after[..end].to_owned();
                stream.drain(..start + 8 + end + 8);
                updated = true;
                let mut pos = 0usize;
                while let Some(rel) = block[pos..].find("\x1b[") {
                    let seq_start = pos + rel;
                    pos = seq_start + 2;
                    let bytes = block.as_bytes();
                    let mut num_end = pos;
                    while num_end < bytes.len() && bytes[num_end].is_ascii_digit() {
                        num_end += 1;
                    }
                    if num_end > pos && block[num_end..].starts_with(";1H") {
                        let row: usize = block[pos..num_end].parse().unwrap_or(0);
                        let cmd_start = num_end + 3;
                        if row >= 1 && row <= screen.len() {
                            if block[cmd_start..].starts_with("\x1b[J") {
                                for line in &mut screen[row - 1..] {
                                    line.clear();
                                }
                                pos = cmd_start + 3;
                            } else if block[cmd_start..].starts_with("\x1b[2K") {
                                let text_start = cmd_start + 4;
                                let mut text_end = block.len();
                                let mut scan = text_start;
                                while let Some(next_esc) = block[scan..].find("\x1b[") {
                                    let cand = scan + next_esc + 2;
                                    let mut d = cand;
                                    while d < bytes.len() && bytes[d].is_ascii_digit() {
                                        d += 1;
                                    }
                                    if d > cand && block[d..].starts_with(";1H") {
                                        text_end = scan + next_esc;
                                        break;
                                    }
                                    scan = cand;
                                }
                                screen[row - 1] = render::clean(&block[text_start..text_end], 4096);
                                pos = text_end;
                            }
                        }
                    }
                }
                if overview {
                    assert!(
                        screen.iter().all(|line| {
                            OVERVIEW_HISTORY.iter().all(|title| !line.contains(title))
                                && (complete_overview_header.get()
                                    || !line.contains(OVERVIEW_CHILD))
                        }),
                        "unverified child or history flashed in Overview:\n{}",
                        screen.join("\n")
                    );
                }
            }
            updated
        };
        let wait_for = |stream: &mut String,
                        screen: &mut [String],
                        rx: &mpsc::Receiver<Vec<u8>>,
                        label: &str,
                        pred: &mut dyn FnMut(&[String]) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                apply_frames(stream, screen);
                if pred(screen) {
                    return;
                }
                let rem = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !rem.is_zero(),
                    "timed out waiting for {label}; screen:\n{}",
                    screen.join("\n")
                );
                if let Ok(chunk) = rx.recv_timeout(rem.min(Duration::from_millis(50))) {
                    stream.push_str(&String::from_utf8_lossy(&chunk));
                }
            }
        };

        type PtyDriver<'a> = dyn FnMut(&mut std::fs::File, &mut String, &mut [String], &mpsc::Receiver<Vec<u8>>)
            + 'a;
        let run_pty = |first_capture_held: bool,
                       stalled_quit: bool,
                       drive: &mut PtyDriver<'_>|
         -> ([usize; 8], String) {
            let _ = std::fs::remove_file(&stall_capture);
            let _ = std::fs::remove_file(stall_capture.with_extension("sampled"));
            let _ = std::fs::remove_file(&capture_entered);
            let _ = std::fs::remove_file(&quota_stage);
            if first_capture_held {
                std::fs::write(&stall_capture, "first").unwrap();
            }
            let _ = std::fs::remove_file(&log);
            let _ = std::fs::remove_file(&counters);
            // A PTY sized like the fixture pane; the child takes it as controlling terminal.
            let master = rustix::pty::openpt(
                rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY,
            )
            .unwrap();
            rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC).unwrap();
            rustix::pty::grantpt(&master).unwrap();
            rustix::pty::unlockpt(&master).unwrap();
            let tty = rustix::pty::ptsname(&master, Vec::new()).unwrap();
            let slave = rustix::fs::open(
                tty.as_c_str(),
                rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .unwrap();
            rustix::termios::tcsetwinsize(
                &slave,
                rustix::termios::Winsize {
                    ws_row: 45,
                    ws_col: 160,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                },
            )
            .unwrap();
            let slave = std::fs::File::from(slave);
            rustix::fs::fcntl_setfl(
                &master,
                rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NONBLOCK,
            )
            .unwrap();
            let mut child_out = std::fs::File::from(master);
            let mut child_in = child_out.try_clone().unwrap();
            let mut pty_cmd = Command::new(&exe);
            pty_cmd
                .args(["--exact", test_name, "--nocapture"])
                .env_clear()
                .env("PATH", &bin)
                .env("HOME", &home)
                .env("TMPDIR", &tmp.0)
                .env("XDG_STATE_HOME", &state)
                .env("TERM", "xterm-256color")
                .env("TMUX_PANE", "%2")
                .env(WATCH_PTY, tty.to_str().unwrap())
                .env("OMS_TEST_REPO", &repo)
                .env("OMS_TEST_EXECUTION", &wt)
                .env("OMS_TEST_COUNTERS", &counters)
                .env("OMS_FAKE_TMUX_LOG", &log)
                .env("OMS_FAKE_ROOM", &room_id)
                .env("OMS_FAKE_ATTEMPT", &attempt_id)
                .env("OMS_FAKE_LEAD_PID", lead_pid.to_string())
                .env("OMS_FAKE_PROMPT_FILE", &prompt_file)
                .env("OMS_FAKE_STALL_CAPTURE", &stall_capture)
                .env("OMS_FAKE_CAPTURE_ENTERED", &capture_entered)
                .env("OMS_FAKE_PROVIDER_MODEL", &model)
                .env("OMS_FAKE_LEAD_WINDOW", &lead_window)
                .env("OMS_FAKE_QUOTA_STAGE", &quota_stage)
                .stdin(slave.try_clone().unwrap())
                .stdout(slave)
                .stderr(Stdio::inherit());
            if codex {
                pty_cmd.env("OMS_TEST_CODEX_LEAD", "1");
            }
            let mut child = pty_cmd.spawn().unwrap();
            std::fs::write(&watch_pid, child.id().to_string()).unwrap();
            // The command holds the last parent copies of the slave.
            drop(pty_cmd);

            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let reaped = std::sync::Arc::new(AtomicBool::new(false));
            let reader_reaped = std::sync::Arc::clone(&reaped);
            let reader_thread = std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut quiet = 0;
                // The master is non-blocking. Read until the child side closes, or until the
                // child was reaped and nothing more arrives.
                while quiet < 5 {
                    match child_out.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            quiet = 0;
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if reader_reaped.load(std::sync::atomic::Ordering::Relaxed) {
                                quiet += 1;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            });

            let mut screen = vec![String::new(); 45];
            let mut stream = String::new();
            drive(&mut child_in, &mut stream, &mut screen, &rx);

            let quit = Instant::now();
            child_in.write_all(b"q").unwrap();
            child_in.flush().unwrap();
            let status = child.wait().unwrap();
            reaped.store(true, std::sync::atomic::Ordering::Relaxed);
            if stalled_quit {
                let entered = std::fs::read_to_string(&capture_entered).unwrap();
                let (_, pid) = entered.split_once('\n').unwrap();
                let pid = pid.parse::<u32>().unwrap();
                assert!(
                    oms_panel_core::terminals::process(pid).is_err(),
                    "stalled capture child {pid} was still alive after Board quit"
                );
                let elapsed = quit.elapsed();
                assert!(
                    stall_capture.exists() && elapsed < Duration::from_secs(2),
                    "stalled capture delayed quit by {elapsed:?}"
                );
                eprintln!("stalled provider: quit-to-exit {elapsed:?}");
            }
            let _ = reader_thread.join();
            assert!(status.success(), "PTY child failed: {status:?}");

            let counter_raw = std::fs::read_to_string(&counters).unwrap();
            let mut parts = counter_raw
                .trim()
                .split(',')
                .map(|p| p.parse::<usize>().unwrap());
            let parsed: [usize; 8] = [
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
            ];
            let tmux_log = std::fs::read_to_string(&log).unwrap();
            (parsed, tmux_log)
        };

        if codex {
            std::fs::write(&lead_window, "@3").unwrap();
            std::fs::write(&prompt_file, format!("› {prompt_line}\n")).unwrap();
            let controls = timing.is_some();
            let mut samples = [Vec::new(), Vec::new()];
            let codex_captures = |command: &str| {
                std::fs::read_to_string(&log)
                    .unwrap()
                    .lines()
                    .filter(|line| *line == command)
                    .count()
            };
            let primary_captures = || codex_captures("capture-pane -p -t %3");
            let recent_captures = || codex_captures("capture-pane -p -J -S -100 -t %3");
            let codex_watch_armed = || {
                let pid = std::fs::read_to_string(&watch_pid).unwrap();
                std::fs::read_dir(format!("/proc/{pid}/fd"))
                    .unwrap()
                    .flatten()
                    .any(|entry| {
                        std::fs::read_link(entry.path())
                            .is_ok_and(|target| target == Path::new("anon_inode:inotify"))
                    })
            };
            for trial in 0..trials {
                let order = if trial.is_multiple_of(2) {
                    [true, false]
                } else {
                    [false, true]
                };
                for armed in order.into_iter().take(if controls { 2 } else { 1 }) {
                    let history = "Codex history fixture";
                    let (next, (child, metadata)) = if overview {
                        (
                            OVERVIEW_CHILD,
                            crate::native_children::seed_codex_overview_rollouts(
                                &codex_home,
                                &repo,
                                turn,
                                OVERVIEW_HISTORY,
                                OVERVIEW_CHILD,
                            ),
                        )
                    } else {
                        let next = "Codex child-only fixture";
                        (
                            next,
                            crate::native_children::seed_codex_watch_rollouts(
                                &codex_home,
                                &repo,
                                turn,
                                history,
                                next,
                            ),
                        )
                    };
                    if child.exists() {
                        std::fs::remove_file(&child).unwrap();
                    }
                    std::fs::set_permissions(
                        child.parent().unwrap(),
                        std::fs::Permissions::from_mode(if armed { 0o700 } else { 0o775 }),
                    )
                    .unwrap();
                    let (_, calls) = run_pty(false, false, &mut |input, stream, screen, rx| {
                        wait_for(stream, screen, rx, "bound Codex overview", &mut |lines| {
                            lines.iter().any(|line| line.contains("@fixture"))
                                && lines.iter().any(|line| line.contains("[Overview]"))
                                && (!overview || codex_watch_armed())
                        });
                        if !overview {
                            input.write_all(b"\t").unwrap();
                            input.flush().unwrap();
                            wait_for(stream, screen, rx, "bound Codex history", &mut |lines| {
                                lines.iter().any(|line| line.contains("[All tasks]"))
                                    && lines.iter().any(|line| line.contains(history))
                                    && lines.iter().all(|line| !line.contains(next))
                            });
                        }
                        assert_eq!(crate::wakeup::socket_path(&repo).exists(), socket_fallback);
                        assert_eq!(codex_watch_armed(), armed);
                        let partial = (overview
                            || matches!(stimulus, Some(CodexWatchStimulus::Partial)))
                        .then(|| {
                            let before = primary_captures();
                            let recent_before = recent_captures();
                            if overview {
                                // The next provider-only refresh changes a
                                // visible footer while keeping the managed Turn.
                                let pane = format!(
                                    "› {prompt_line}\n  {OVERVIEW_PARTIAL_MODEL} high · Context 95% left\n← for agents · ? for shortcuts\n"
                                );
                                assert_eq!(
                                    crate::orchestrator::codex_model_from_chrome(&pane).as_deref(),
                                    Some(OVERVIEW_PARTIAL_MODEL)
                                );
                                std::fs::write(&prompt_file, pane).unwrap();
                            }
                            let mut file = std::fs::File::create(&child).unwrap();
                            let boundary = if overview {
                                assert!(metadata.ends_with('\n'));
                                metadata.len() - 1
                            } else {
                                metadata.len() / 2
                            };
                            let (prefix, rest) = metadata.split_at(boundary);
                            file.write_all(prefix.as_bytes()).unwrap();
                            // The second recent capture follows the actual rollout scan.
                            wait_for(
                                stream,
                                screen,
                                rx,
                                "partial Codex header scanned",
                                &mut |lines| {
                                    assert!(lines.iter().all(|line| !line.contains(next)));
                                    recent_captures() >= recent_before + 2
                                        && (!overview
                                            || lines.iter().any(|line| {
                                                line.contains(OVERVIEW_PARTIAL_MODEL)
                                                    && line.contains("@fixture")
                                            }))
                                },
                            );
                            assert_eq!(primary_captures(), before + 1);
                            (file, rest)
                        });
                        let prior = primary_captures();
                        let started = Instant::now();
                        if let Some((mut file, remainder)) = partial {
                            file.write_all(remainder.as_bytes()).unwrap();
                            if overview {
                                complete_overview_header.set(true);
                            }
                        } else {
                            std::fs::write(&child, &metadata).unwrap();
                        }
                        let wrote = started.elapsed();
                        let upper = match (armed, socket_fallback) {
                            (true, true) => Duration::from_secs(8),
                            (true, false) => Duration::from_secs(3),
                            (false, false) => Duration::from_secs(9),
                            (false, true) => Duration::from_secs(36),
                        };
                        let mut capture_at = None;
                        loop {
                            if capture_at.is_none() && primary_captures() > prior {
                                capture_at = Some(started.elapsed());
                            }
                            apply_frames(stream, screen);
                            if screen.iter().any(|line| line.contains(next)) {
                                break;
                            }
                            assert!(
                                started.elapsed() < upper,
                                "Codex child paint exceeded {upper:?}"
                            );
                            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(10)) {
                                stream.push_str(&String::from_utf8_lossy(&chunk));
                            }
                        }
                        let elapsed = started.elapsed();
                        let lower = if socket_fallback { 20 } else { 3 };
                        assert!(
                            elapsed < upper && (armed || elapsed >= Duration::from_secs(lower))
                        );
                        assert_eq!(primary_captures(), prior + 1);
                        if overview {
                            assert!(screen.iter().any(|line| line.contains("[Overview]")));
                            let row = screen.iter().find(|line| line.contains(next)).unwrap();
                            assert!(
                                row.contains("unknown") && !row.contains("seen "),
                                "a fresh header does not prove activity: {row}",
                            );
                        }
                        samples[usize::from(!armed)].push(elapsed);
                        if controls {
                            eprintln!(
                                "CODEX_ROSTER_SAMPLE trial={} watch={} write_us={} paint_us={} capture_us={}",
                                trial + 1,
                                armed,
                                wrote.as_micros(),
                                elapsed.as_micros(),
                                capture_at
                                    .expect("changed frame without a capture")
                                    .as_micros()
                            );
                        }
                    });
                    assert!(!calls.contains("UNEXPECTED:"), "{calls}");
                }
            }
            if controls {
                for (label, group) in ["candidate", "fallback"].into_iter().zip(&mut samples) {
                    group.sort_unstable();
                    let percentile =
                        |pct: usize| group[(group.len() * pct).div_ceil(100) - 1].as_micros();
                    eprintln!(
                        "CODEX_ROSTER_SUMMARY mode={} case={label} count={} p50_us={} p95_us={}",
                        timing.as_deref().unwrap(),
                        group.len(),
                        percentile(50),
                        percentile(95)
                    );
                    if label == "candidate" {
                        assert!(percentile(95) <= 2_000_000);
                    }
                }
            }
            let _ = holder.kill();
            let _ = holder.wait();
            return;
        }

        let mutating_out = Command::new(&fake_tmux)
            .args(["send-keys", "-t", "%4", "Enter"])
            .env_clear()
            .env("OMS_FAKE_TMUX_LOG", &log)
            .env("OMS_TEST_REPO", &repo)
            .env("OMS_FAKE_ROOM", &room_id)
            .env("OMS_FAKE_ATTEMPT", &attempt_id)
            .env("OMS_FAKE_LEAD_PID", lead_pid.to_string())
            .env("OMS_FAKE_PROMPT_FILE", &prompt_file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!mutating_out.status.success());
        assert!(
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("UNEXPECTED:")
        );
        let _ = std::fs::remove_file(&log);

        // Run 1: cold Main status, input-only redraw, a quiet 5s tick, worker reply wake
        // and explicit refresh. Without a row open, child transcripts get only the
        // bounded per-refresh usage suffix reads asserted below.
        let (pre_click_counters, pre_click_tmux_log) = run_pty(
            false,
            false,
            &mut |child_in, stream, screen, rx| {
                wait_for(stream, screen, rx, "council fixture board", &mut |s| {
                    s.iter()
                        .any(|line| line.contains("Overall Main") && line.contains("working"))
                });
                child_in.write_all(b"c").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "council positions", &mut |s| {
                    s.iter().any(|line| line.contains("Council fixture title"))
                        && s.iter().any(|line| line.contains("COUNCIL_POSITION_ONE"))
                        && s.iter().any(|line| line.contains("COUNCIL_POSITION_TWO"))
                });
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "council close", &mut |s| {
                    s.iter().all(|line| !line.contains("COUNCIL_POSITION_ONE"))
                });
                wait_for(stream, screen, rx, "refresh-only initial board", &mut |s| {
                    s[0].contains("OMS · repo")
                        && s.iter()
                            .any(|l| l.contains("Overall Main") && l.contains("working"))
                        && s.iter().any(|l| l.contains("Codex · GPT-6.2-Sol"))
                        && s.iter().any(|l| l.contains("Branch wt · Worktree repo-wt"))
                        && s.iter().any(|l| l.contains("2 past (Tab all)"))
                        && s.iter().any(|l| l.contains("Silent Legacy"))
                        && !s.iter().any(|l| l.contains("Refreshing Board"))
                });
                assert!(
                    !screen.join("\n").contains("UNPROVED_SILENT_LEGACY_REPLY"),
                    "passive refresh leaked unproved child transcript"
                );
                assert!(crate::wakeup::socket_path(&repo).exists());
                let count_tmux = || std::fs::read_to_string(&log).unwrap().lines().count();
                let before = count_tmux();
                child_in.write_all(b"?").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "visual help", &mut |s| {
                    s.iter().any(|l| l.contains("Keyboard Shortcuts"))
                });
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "visual help close", &mut |s| {
                    s.iter().all(|l| !l.contains("Keyboard Shortcuts"))
                });
                child_in.write_all(b"/uD\r").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "visual filter", &mut |s| {
                    s.iter().any(|l| l.contains("Filter /uD"))
                });
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "visual filter clear", &mut |s| {
                    s.iter().all(|l| !l.contains("Filter /"))
                });
                assert_eq!(
                    count_tmux(),
                    before,
                    "visual keys must stay on the cached frame"
                );

                // Wait across the former periodic tick to test the absence of
                // unchanged work, not an assumed provider or scheduler delay.
                let quiet_until = Instant::now() + super::REFRESH_INTERVAL;
                while let Some(wait) = quiet_until.checked_duration_since(Instant::now()) {
                    if let Ok(chunk) = rx.recv_timeout(wait) {
                        stream.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    assert_eq!(
                        count_tmux(),
                        before,
                        "no managed pane rebuild after five quiet seconds"
                    );
                }

                child_in.write_all(b"w").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "fenced Weekly open", &mut |s| {
                    s.iter().any(|l| l.contains("Weekly usage"))
                });
                assert_eq!(
                    count_tmux(),
                    before + 1,
                    "Weekly open sampled the exact pane"
                );
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "cached Weekly close", &mut |s| {
                    s[0].contains("OMS · repo")
                });
                assert_eq!(
                    count_tmux(),
                    before + 1,
                    "Weekly close used the cached frame"
                );

                let count_observations = || {
                    std::fs::read_to_string(&log)
                        .unwrap()
                        .lines()
                        .filter(|line| *line == "capture-pane -p -t %4")
                        .count()
                };
                let before_wake = count_observations();
                let result = fixture_command()
                    .env(REPLY_ENV, "1")
                    .env("OMS_FAKE_ROOM", &room_id)
                    .env("OMS_FAKE_ATTEMPT", &attempt_id)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "worker reply fixture: {:?}",
                    result
                );
                wait_for(stream, screen, rx, "worker reply wake", &mut |s| {
                    s[0].contains("1 attention") && count_observations() > before_wake
                });
                assert_eq!(
                    count_observations(),
                    before_wake + 1,
                    "reply burst coalesced"
                );

                let refresh_started = Instant::now();
                std::fs::write(&stall_capture, "").unwrap();
                set_model("Refresh resumed");
                child_in.write_all(b"u").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "stalled provider capture", &mut |_| {
                    capture_entered.exists()
                });
                assert!(screen.iter().any(|line| line.contains("Fixture initial")));
                let key_started = Instant::now();
                child_in.write_all(b"?").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "help during capture", &mut |s| {
                    s.iter().any(|l| l.contains("Keyboard Shortcuts"))
                });
                let key_repaint = key_started.elapsed();
                assert!(
                    stall_capture.exists() && key_repaint < Duration::from_secs(2),
                    "stalled capture delayed help by {key_repaint:?}"
                );
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "close help during capture", &mut |s| {
                    s.iter().all(|line| !line.contains("Keyboard Shortcuts"))
                        && s.iter().any(|line| line.contains("Fixture initial"))
                });
                std::fs::remove_file(&stall_capture).unwrap();
                let release = Instant::now();
                wait_for(stream, screen, rx, "fresh provider observation", &mut |s| {
                    s.iter().any(|line| line.contains("Refresh resumed"))
                });
                eprintln!(
                    "stalled provider: help key-to-repaint {key_repaint:?}, refresh-to-repaint {:?}, release-to-repaint {:?}",
                    refresh_started.elapsed(),
                    release.elapsed()
                );
            },
        );
        assert!(!pre_click_tmux_log.contains("UNEXPECTED:"));
        let [
            pre_custody,
            pre_scans,
            pre_strict_reads,
            pre_legacy_reads,
            pre_leg_custody,
            pre_leg_parent,
            pre_leg_cand,
            pre_leg_child,
        ] = pre_click_counters;
        let run1_captures_p = pre_click_tmux_log
            .lines()
            .filter(|l| *l == "capture-pane -p -t %4")
            .count();
        let run1_captures_recent = pre_click_tmux_log
            .lines()
            .filter(|l| *l == "capture-pane -p -J -S -100 -t %4")
            .count();
        assert!(run1_captures_p >= 2);
        assert_eq!(run1_captures_p, run1_captures_recent);
        assert_eq!(
            (pre_leg_custody, pre_leg_parent, pre_leg_cand, pre_leg_child),
            (1, run1_captures_p, 0, 0),
            "expected cold-only custody check (1) and exact 1 parent read per observe, 0 candidates, 0 child reads; log:\n{pre_click_tmux_log}"
        );
        // Usage is a bounded per-refresh scan: each accepted Board refresh does
        // 2 strict custody checks, 1 parent scan and one suffix read per unique
        // painted child, capped at orchestrator's AGY_USAGE_ROWS (3). Run 1
        // accepts every observe (cold, reply wake, `u`), one lead capture each.
        let accepted = run1_captures_p;
        let painted = lead_rows
            .iter()
            .filter_map(|row| {
                let body = row["content"]
                    .as_str()?
                    .strip_prefix("Created the following subagent:\n")?;
                let created: Value = serde_json::from_str(body).ok()?;
                created["conversationId"].as_str().map(str::to_owned)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            .min(3);
        assert_eq!(
            (pre_custody, pre_scans, pre_strict_reads, pre_legacy_reads),
            (2 * accepted, accepted, painted * accepted, 0),
            "before Click, each of {accepted} accepted refreshes does 2 strict custody checks, 1 usage scan and \
             1 bounded suffix read per {painted} painted child (never a whole transcript); no legacy reads"
        );
        assert!(pre_strict_reads <= 3 * accepted);

        // Run 2: Click -> Escape -> Enter -> Inode-swap close -> Re-click -> Live Turn-change close -> 0775 Unbound fallback & Silent Legacy Unavailable.
        let (run2_counters, tmux_log) =
            run_pty(false, false, &mut |child_in, stream, screen, rx| {
                let list_restored = |screen: &[String]| {
                    screen
                        .last()
                        .is_some_and(|line| line.contains("Enter Open"))
                        && screen.iter().all(|line| {
                            !line.contains("Select an orchestrator, worker, or council")
                        })
                };
                // 1. Wait for initial fresh board, then show all tasks.
                wait_for(stream, screen, rx, "initial fresh board", &mut |s| {
                    list_restored(s)
                        && s.iter().any(|l| l.contains("2 past (Tab all)"))
                        && !s.iter().any(|l| l.contains("Refreshing Board"))
                });
                child_in.write_all(b"\t").unwrap();
                child_in.flush().unwrap();
                wait_for(
                    stream,
                    screen,
                    rx,
                    "all tasks showing all 3 children",
                    &mut |s| {
                        s.iter().any(|l| l.contains("[All tasks]"))
                            && s.iter().any(|l| l.contains("Bound Researcher"))
                            && s.iter().any(|l| l.contains("Sibling Reviewer"))
                            && s.iter().any(|l| l.contains("Silent Legacy"))
                    },
                );
                child_in.write_all(b"\t").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "tab returns to overview", &mut |s| {
                    s.iter().any(|l| l.contains("[Overview]"))
                        && s.iter().any(|l| l.contains("2 past (Tab all)"))
                });
                child_in.write_all(b"\t").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "all tasks restored", &mut |s| {
                    s.iter().any(|l| l.contains("[All tasks]"))
                        && s.iter().any(|l| l.contains("Bound Researcher"))
                });
                let bound_row = screen
                    .iter()
                    .position(|l| l.contains("Bound Researcher"))
                    .unwrap();

                // 2. SGR mouse Click on Bound Researcher opens direct child chat (no parent summary).
                write!(child_in, "\x1b[<0;3;{}M", bound_row + 1).unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "click child chat", &mut |s| {
                    s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                });
                let clicked_text = screen.join("\n");
                assert!(clicked_text.contains("Bound Researcher · subagent"));
                assert!(!clicked_text.contains("PARENT_DELIVERED_SUMMARY_A"));
                assert!(!clicked_text.contains(super::PARENT_SUMMARY));
                assert!(!clicked_text.contains("SIBLING_NEVER_READ"));
                assert!(!clicked_text.contains("UNPROVED_SILENT_LEGACY_REPLY"));
                assert!(!clicked_text.contains("HIDDEN_THINKING"));

                // 3. Escape closes dock.
                std::thread::sleep(Duration::from_millis(120));
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "escape closes click dock", &mut |s| {
                    list_restored(s) && !s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                });

                // 4. Enter re-opens selected Bound Researcher direct child chat.
                child_in.write_all(b"\r").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "enter child chat", &mut |s| {
                    s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                });

                // 5. While open, swap child_a transcript inode and refresh with 'u': recheck_open_agy_chat closes dock (0 child reads).
                let before_open_inval = count_captures();
                let child_a_full = brain
                    .join(child_a)
                    .join(".system_generated/logs/transcript_full.jsonl");
                let swap_tmp = tmp.0.join("swap-child-a.jsonl");
                std::fs::copy(&child_a_full, &swap_tmp).unwrap();
                std::fs::set_permissions(&swap_tmp, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                std::fs::rename(&swap_tmp, &child_a_full).unwrap();
                child_in.write_all(b"u").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "open dock invalidation", &mut |s| {
                    count_captures() > before_open_inval
                        && s.iter().any(|l| l.contains("Transcript unavailable"))
                        && list_restored(s)
                        && !s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                });

                // 6. Re-open Bound Researcher by Click, then advance lead Turn while open: live refresh closes dock.
                let rebound_row = screen
                    .iter()
                    .position(|l| l.contains("Bound Researcher"))
                    .unwrap();
                write!(child_in, "\x1b[<0;3;{}M", rebound_row + 1).unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "reopen after swap", &mut |s| {
                    s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                });
                let before_turn_inval = count_captures();
                write_prompt(&next_prompt_line);
                child_in.write_all(b"u").unwrap();
                child_in.flush().unwrap();
                wait_for(
                    stream,
                    screen,
                    rx,
                    "live turn change closes dock",
                    &mut |s| {
                        count_captures() > before_turn_inval
                            && s.iter().any(|l| l.contains("Transcript unavailable"))
                            && list_restored(s)
                            && !s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                    },
                );
                let before_restore = count_captures();
                write_prompt(&prompt_line);
                set_model("Turn restored");
                child_in.write_all(b"\tu").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "restore lead turn", &mut |s| {
                    count_captures() > before_restore
                        && s.iter().any(|l| l.contains("Turn restored"))
                        && s.iter().any(|l| l.contains("[Overview]"))
                        && !s.iter().any(|l| l.contains("Refreshing Board"))
                });

                // 7. Chmod .git and wt to 0o775 and press 'u' to refresh into legacy mode.
                let before_captures = count_captures();
                std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o775))
                    .unwrap();
                std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o775)).unwrap();
                set_model("Legacy mode");
                child_in.write_all(b"u").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "legacy 0775 refresh", &mut |s| {
                    count_captures() > before_captures
                        && s.iter().any(|l| l.contains("Legacy mode"))
                        && s.iter().any(|l| l.contains("[Overview]"))
                        && !s.iter().any(|l| l.contains("Refreshing Board"))
                });
                child_in.write_all(b"\t").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "show accepted legacy rows", &mut |s| {
                    s.iter().any(|line| line.contains("[All tasks]"))
                        && s.iter().any(|line| line.contains("Bound Researcher"))
                        && s.iter().any(|line| line.contains("Silent Legacy"))
                });
                let legacy_row = screen
                    .iter()
                    .position(|l| l.contains("Bound Researcher"))
                    .unwrap();

                // 7. Click on 0775 Bound Researcher opens parent-delivered summary fallback (0 child reads).
                write!(child_in, "\x1b[<0;3;{}M", legacy_row + 1).unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "legacy fallback dock", &mut |s| {
                    s.iter().any(|l| l.contains("PARENT_DELIVERED_SUMMARY_A"))
                });
                let fallback_text = screen.join("\n");
                assert!(fallback_text.contains("parent summary"));
                assert!(fallback_text.contains(super::PARENT_SUMMARY));
                assert!(!fallback_text.contains("OWN_CHILD_DIRECT_REPLY"));
                assert!(!fallback_text.contains("SIBLING_NEVER_READ"));
                assert!(!fallback_text.contains("UNPROVED_SILENT_LEGACY_REPLY"));

                // 8. Escape closes dock; Click on Silent Legacy (no parent delivery) shows "Transcript unavailable" (0 child reads).
                std::thread::sleep(Duration::from_millis(120));
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(
                    stream,
                    screen,
                    rx,
                    "escape closes fallback dock",
                    &mut |s| {
                        list_restored(s)
                            && !s.iter().any(|l| l.contains("PARENT_DELIVERED_SUMMARY_A"))
                    },
                );
                let silent_row = screen
                    .iter()
                    .position(|l| l.contains("Silent Legacy"))
                    .unwrap();
                write!(child_in, "\x1b[<0;3;{}M", silent_row + 1).unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "silent legacy unavailable", &mut |s| {
                    s.iter().any(|l| l.contains("Transcript unavailable"))
                });
                let silent_text = screen.join("\n");
                assert!(list_restored(screen));
                assert!(!silent_text.contains("UNPROVED_SILENT_LEGACY_REPLY"));
                assert!(!silent_text.contains(super::PARENT_SUMMARY));
                assert!(!silent_text.contains("PARENT_DELIVERED_SUMMARY_A"));
            });

        let [
            custody,
            scans,
            strict_reads,
            legacy_reads,
            _leg_custody,
            _leg_parent,
            leg_cand,
            leg_child,
        ] = run2_counters;
        // Verify 0 real tmux calls & exact persisted child read counters:
        // legacy_child=0, strict_reads = 3 dock opens (Click #1 + Enter #2 + Click #3)
        // plus one usage suffix read per painted child on each usage refresh,
        // leg_cand=2 (ONLY the 2 lead transcript checks when Turn switched to NEW_TURN),
        // and protected rechecks (custody >= 7, scans >= 3) on top of usage scans.
        assert!(!tmux_log.is_empty());
        assert!(!tmux_log.contains("UNEXPECTED:"));
        assert!(tmux_log.contains("list-panes -t %2"));
        assert!(tmux_log.contains("capture-pane -p -J -S -100 -t %4"));
        let observes = tmux_log
            .lines()
            .filter(|l| *l == "capture-pane -p -t %4")
            .count();
        // Every observe but the NEW_TURN drift (absent from the lead transcript)
        // and the 0775 legacy refresh is a usage refresh; those two read nothing.
        let usage_refreshes = observes - 2;
        let dock_reads = 3;
        assert!(
            custody >= 7 + 2 * usage_refreshes && scans >= 3 + usage_refreshes,
            "custody={custody} scans={scans} usage_refreshes={usage_refreshes}"
        );
        assert_eq!(
            (legacy_reads, strict_reads, leg_cand, leg_child),
            (0, dock_reads + painted * usage_refreshes, 2, 0),
            "expected legacy_reads=0, strict_reads = {dock_reads} dock opens + {painted} painted children x \
             {usage_refreshes} usage refreshes ({observes} observes less Turn drift and 0775), leg_cand=2, \
             leg_child=0; custody={custody} scans={scans}"
        );
        assert_eq!(
            legacy_reads + strict_reads,
            dock_reads + painted * usage_refreshes
        );

        std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o700)).unwrap();
        let entered = |ticket: &str| {
            std::fs::read_to_string(&capture_entered).is_ok_and(|value| {
                value
                    .split_once('\n')
                    .is_some_and(|(value, _)| value == ticket)
            })
        };
        let samples = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|line| line.starts_with("list-panes -t %2 -F "))
                .count()
        };
        let repeat_wakes =
            |stream: &mut String, screen: &mut [String], rx: &mpsc::Receiver<Vec<u8>>| {
                for _ in 0..2 {
                    let before = samples();
                    crate::wakeup::notify_wakeup(&repo);
                    wait_for(stream, screen, rx, "same-scope wake sampled", &mut |_| {
                        samples() > before
                    });
                }
            };
        let (stalled, stalled_log) = run_pty(false, true, &mut |child_in, stream, screen, rx| {
            wait_for(
                stream,
                screen,
                rx,
                "last-good frame before generation drift",
                &mut |s| s.iter().any(|line| line.contains("Legacy mode")),
            );
            std::fs::write(&stall_capture, "old-pane").unwrap();
            set_model("Discarded pane model");
            child_in.write_all(b"u").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "old provider pane capture", &mut |_| {
                entered("old-pane")
            });
            repeat_wakes(stream, screen, rx);
            assert!(screen.iter().any(|line| line.contains("Legacy mode")));
            std::fs::write(&lead_window, "@5").unwrap();
            std::fs::write(&stall_capture, "new-pane").unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "old provider pane rejected while follow-up held",
                &mut |s| {
                    assert!(!s.iter().any(|line| line.contains("Discarded pane model")));
                    entered("new-pane")
                        && s[0].contains("OMS · repo")
                        && s.iter().all(|line| !line.contains("Legacy mode"))
                },
            );
            assert!(!stream.contains("Discarded pane model"));
        });
        assert!(!stalled_log.contains("UNEXPECTED:"));
        // Every accepted refresh keeps Run 1's usage cost. The window-move frame
        // reads its recent screen, then its fresh pane census fails the expected
        // generation, so it adds no custody check, scan or read; the follow-up
        // stays held in its first capture until quit.
        let stalled_count = |line: &str| stalled_log.lines().filter(|l| *l == line).count();
        let stalled_accepted = stalled_count("capture-pane -p -t %4") - 2;
        assert_eq!(
            stalled_count("capture-pane -p -J -S -100 -t %4"),
            stalled_accepted + 1
        );
        // Each finished refresh loads, takes its one census, then reloads to
        // fence the render; the held follow-up has only loaded.
        let censuses = stalled_log
            .lines()
            .filter(|l| l.starts_with("list-panes -s -t %2 -F "))
            .count();
        assert_eq!(censuses, 3 * (stalled_accepted + 1) + 1);
        assert_eq!(
            stalled[..4],
            [
                2 * stalled_accepted,
                stalled_accepted,
                painted * stalled_accepted,
                0
            ],
            "{stalled_accepted} accepted refreshes each read usage; the moved-pane frame reads none"
        );

        set_model("First same-scope observation");
        let (_, repeated_log) = run_pty(true, true, &mut |child_in, stream, screen, rx| {
            wait_for(stream, screen, rx, "first capture held", &mut |_| {
                entered("first")
            });
            repeat_wakes(stream, screen, rx);
            // Release only the first capture. The deferred follow-up now waits
            // for its own ticket, with Board actions still Pending.
            std::fs::write(&stall_capture, "follow-up").unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "first accepted projection while follow-up held",
                &mut |s| {
                    entered("follow-up")
                        && s.iter()
                            .any(|line| line.contains("First same-scope observation"))
                },
            );
            child_in.write_all(b"?").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "help with follow-up held", &mut |s| {
                s.iter().any(|line| line.contains("Keyboard Shortcuts"))
            });
            child_in.write_all(b"\x1b").unwrap();
            child_in.flush().unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "close help while follow-up held",
                &mut |s| s.iter().all(|line| !line.contains("Keyboard Shortcuts")),
            );
        });
        assert!(!repeated_log.contains("UNEXPECTED:"));

        let (_, quota_log) = run_pty(false, true, &mut |child_in, stream, screen, rx| {
            wait_for(stream, screen, rx, "initial quota Board", &mut |s| {
                s.iter()
                    .any(|line| line.contains("First same-scope observation"))
            });
            child_in.write_all(b"w").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "fresh Weekly overlay", &mut |s| {
                s.iter().any(|line| line.contains("U Refresh quota"))
            });
            std::fs::write(&quota_stage, "armed").unwrap();
            std::fs::write(&stall_capture, "quota").unwrap();
            child_in.write_all(b"U").unwrap();
            child_in.flush().unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "authorized quota completion stalled",
                &mut |_| {
                    std::fs::read_to_string(&capture_entered)
                        .is_ok_and(|row| row.starts_with("quota\n"))
                },
            );
            child_in.write_all(b"\x1b").unwrap();
            child_in.flush().unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "close Weekly while completion stalled",
                &mut |s| s[0].contains("OMS · repo"),
            );
            child_in.write_all(b"?").unwrap();
            child_in.flush().unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "help while quota completion stalled",
                &mut |s| s.iter().any(|line| line.contains("Keyboard Shortcuts")),
            );
            child_in.write_all(b"\x1b").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "close Help before quitting", &mut |s| {
                s.iter().all(|line| !line.contains("Keyboard Shortcuts"))
            });
        });
        assert!(quota_log.contains("send-keys -t %4 Enter"));
        assert!(
            !quota_log.contains("send-keys -t %4 Escape") && !quota_log.contains("UNEXPECTED:")
        );

        // Hold both captures in turn so the targeted result arrives while the
        // same-scope Board is still waiting for an ordinary refresh.
        let child_d = "55555555-5555-5555-5555-eeeeeeeeeeee";
        let participant = |id| crate::native_children::participant_id("antigravity", id);
        assert!(
            [child_a, child_b, child_c]
                .into_iter()
                .all(|id| participant(id) < participant(child_d))
        );
        let mut detail_lead = lead_rows;
        let child_d_call = detail_lead.len();
        detail_lead.extend([
            json!({"step_index":9,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"spawning d","tool_calls":[{"id":"td","name":"invoke_subagent","args":{"Prompt":"count fixture tokens","Role":"Fourth Usage","TypeName":"general"}}]}),
            json!({"step_index":10,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-11-18T18:01:02Z","content":format!("Created the following subagent:\n{:#}", json!({"conversationId":child_d,"workspaceUris":[repo_uri]}))}),
        ]);
        write_jsonl(lead_id, "transcript.jsonl", &detail_lead);
        write_jsonl(lead_id, "transcript_full.jsonl", &detail_lead);
        write_jsonl(
            child_d,
            "transcript_full.jsonl",
            &[
                json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-11-18T18:01:03Z","model":"gemini","input_tokens":1200,"cache_read_tokens":0,"output_tokens":40}),
            ],
        );
        let detail_visible = |screen: &[String]| {
            screen
                .iter()
                .any(|line| line.contains("Call Detail · Fourth Usage"))
        };
        let has_detail_usage = |screen: &[String]| {
            screen
                .iter()
                .any(|line| line.contains("1200 in") && line.contains("40 out"))
        };
        let (detail_counts, detail_log) =
            run_pty(false, false, &mut |child_in, stream, screen, rx| {
                wait_for(
                    stream,
                    screen,
                    rx,
                    "fourth child on fresh Board",
                    &mut |s| s.iter().any(|line| line.contains("Fourth Usage")),
                );
                child_in.write_all(b"/Fourth Usage\r\x1b[B").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "filter fourth child", &mut |s| {
                    s.iter().any(|line| line.contains("Filter /Fourth Usage"))
                });

                std::fs::write(&stall_capture, "detail").unwrap();
                child_in.write_all(b"D").unwrap();
                child_in.flush().unwrap();
                wait_for(
                    stream,
                    screen,
                    rx,
                    "selected detail and targeted read",
                    &mut |s| detail_visible(s) && entered("detail"),
                );
                assert!(!has_detail_usage(screen));
                repeat_wakes(stream, screen, rx);
                std::fs::write(&stall_capture, "detail-follow-up").unwrap();
                wait_for(stream, screen, rx, "ordinary refresh held", &mut |_| {
                    entered("detail-follow-up")
                });
                wait_for(
                    stream,
                    screen,
                    rx,
                    "targeted usage during follow-up",
                    &mut |s| detail_visible(s) && has_detail_usage(s),
                );

                detail_lead[child_d_call]["tool_calls"][0]["args"]["Role"] =
                    json!("Fourth Usage Refreshed");
                write_jsonl(lead_id, "transcript.jsonl", &detail_lead);
                write_jsonl(lead_id, "transcript_full.jsonl", &detail_lead);
                std::fs::remove_file(&stall_capture).unwrap();
                wait_for(
                    stream,
                    screen,
                    rx,
                    "usage in fresh ordinary frame",
                    &mut |s| {
                        has_detail_usage(s)
                            && s.iter()
                                .any(|line| line.contains("Call Detail · Fourth Usage Refreshed"))
                    },
                );
                child_in.write_all(b"\x1b").unwrap();
                child_in.flush().unwrap();
                wait_for(
                    stream,
                    screen,
                    rx,
                    "detail closed and usage hidden",
                    &mut |s| !detail_visible(s) && !has_detail_usage(s),
                );
            });
        assert!(!detail_log.contains("UNEXPECTED:"), "{detail_log}");
        assert_eq!(detail_counts[2], 3 * detail_counts[1] + 1);
        assert_eq!(detail_counts[3], 0);

        // A markerless typed Turn opens the exact bounded parent roster.
        // It starts empty so only a provider-native write can reveal its first child.
        let transcript = brain
            .join(lead_id)
            .join(".system_generated/logs/transcript.jsonl");
        let prepare_empty_roster = |markerless: bool| {
            std::fs::write(&lead_window, "@4").unwrap();
            if markerless {
                std::fs::write(&prompt_file, "Antigravity ready\n").unwrap();
            } else {
                write_prompt(&prompt_line);
            }
            write_jsonl(lead_id, "transcript.jsonl", &detail_lead[..1]);
        };
        let append = |path: &Path, rows: &[Value]| {
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            for row in rows {
                writeln!(&mut file, "{row}").unwrap();
            }
        };
        let start_child = |index| {
            let mut spawn = [detail_lead[5].clone(), detail_lead[6].clone()];
            spawn[0]["step_index"] = json!(index);
            spawn[1]["step_index"] = json!(index + 1);
            spawn
        };
        let has_first_child =
            |screen: &[String]| screen.iter().any(|line| line.contains("Bound Researcher"));
        let count_observations = || {
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .filter(|line| *line == "capture-pane -p -t %4")
                .count()
        };
        let watch_armed = || {
            let pid = std::fs::read_to_string(&watch_pid).unwrap();
            std::fs::read_dir(format!("/proc/{pid}/fd"))
                .unwrap()
                .flatten()
                .any(|fd| {
                    std::fs::read_link(fd.path())
                        .is_ok_and(|target| target == Path::new("anon_inode:inotify"))
                })
        };
        let quiet = |rx: &mpsc::Receiver<Vec<u8>>, duration| {
            let snapshot = std::fs::read_to_string(&log).unwrap();
            let until = Instant::now() + duration;
            while let Some(wait) = until.checked_duration_since(Instant::now()) {
                let _ = rx.recv_timeout(wait);
                assert_eq!(std::fs::read_to_string(&log).unwrap(), snapshot);
            }
        };
        prepare_empty_roster(true);
        set_model("First child fixture");
        let (_, first_child_log) = run_pty(false, false, &mut |_, stream, screen, rx| {
            // A markerless recovered child proves its presence, with no trusted role.
            let markerless_child = |s: &[String]| {
                s.iter()
                    .any(|line| line.contains("Worker") && line.contains("work unreported"))
            };
            wait_for(stream, screen, rx, "accepted empty roster", &mut |s| {
                s.iter().any(|line| line.contains("First child fixture")) && !markerless_child(s)
            });
            assert!(
                watch_armed(),
                "no watch on the accepted markerless empty roster"
            );
            let foreign = brain
                .join(child_b)
                .join(".system_generated/logs/transcript.jsonl");
            append(&foreign, &start_child(1));
            // No key, OMS notification, or periodic observation during this quiet interval.
            quiet(rx, super::REFRESH_INTERVAL);
            let captures = count_observations();
            let began = Instant::now();
            append(&transcript, &start_child(1));
            wait_for(stream, screen, rx, "native wake capture", &mut |_| {
                count_observations() > captures
            });
            let captured = Instant::now();
            wait_for(stream, screen, rx, "provider-only first child", &mut |s| {
                markerless_child(s)
            });
            let elapsed = began.elapsed();
            assert!(elapsed < super::REFRESH_INTERVAL, "{elapsed:?}");
            assert_eq!(count_observations(), captures + 1);
            eprintln!(
                "synthetic AGY append-to-first-row {elapsed:?}, observed capture-to-row {:?}",
                captured.elapsed()
            );
        });
        assert!(!first_child_log.contains("UNEXPECTED:"));

        prepare_empty_roster(false);
        set_model("Native burst fixture");
        let (_, native_burst_log) = run_pty(false, false, &mut |_, stream, screen, rx| {
            wait_for(
                stream,
                screen,
                rx,
                "accepted empty burst roster",
                &mut |s| s.iter().any(|line| line.contains("Native burst fixture")),
            );
            assert!(
                watch_armed(),
                "no watch on the accepted anchored empty roster"
            );
            let first = count_observations();
            set_model("Accepted empty sample");
            std::fs::write(&stall_capture, "native-after-sample").unwrap();
            let neutral = |index| {
                json!({"step_index":index,"source":"MODEL","type":"PLANNER_RESPONSE",
                    "status":"DONE","content":"fixture progress"})
            };
            append(&transcript, &[neutral(1)]);
            wait_for(
                stream,
                screen,
                rx,
                "native refresh held after empty sample",
                &mut |_| entered("native-after-sample"),
            );
            for index in 2..22 {
                append(&transcript, &[neutral(index)]);
            }
            append(&transcript, &start_child(22));
            assert_eq!(count_observations(), first + 1);
            set_model("Native burst fixture");
            std::fs::write(&stall_capture, "native-follow-up").unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "accepted empty frame while follow-up held",
                &mut |s| {
                    entered("native-follow-up")
                        && s.iter().any(|line| line.contains("Accepted empty sample"))
                        && !has_first_child(s)
                },
            );
            std::fs::remove_file(&stall_capture).unwrap();
            wait_for(
                stream,
                screen,
                rx,
                "authoritative first child after burst",
                &mut |s| has_first_child(s),
            );
            quiet(rx, Duration::from_millis(1_100));
            assert_eq!(count_observations(), first + 2);

            // Re-arm from a newly accepted empty roster, then remove the Lead
            // from the verified pane census. Its old inode cannot wake the Board.
            prepare_empty_roster(false);
            crate::wakeup::notify_wakeup(&repo);
            wait_for(stream, screen, rx, "rearmed empty roster", &mut |s| {
                s.iter().any(|line| line.contains("Native burst fixture")) && !has_first_child(s)
            });
            std::fs::write(&lead_window, "absent").unwrap();
            crate::wakeup::notify_wakeup(&repo);
            wait_for(stream, screen, rx, "retired AGY scope", &mut |s| {
                s.iter().all(|line| !line.contains("Native burst fixture")) && !has_first_child(s)
            });
            append(&transcript, &start_child(1));
            quiet(rx, Duration::from_millis(1_100));
        });
        assert!(!native_burst_log.contains("UNEXPECTED:"));

        let _ = holder.kill();
        let _ = holder.wait();
    }

    #[test]
    fn board_key_and_refresh_paths_reconcile_selection_by_call_identity() {
        use crate::input::Key;
        let make_call = |id: &str, attempt: &str, title: &str, state: &str| {
            serde_json::json!({
                "participant": id,
                "attempt": attempt,
                "attempt_id": attempt,
                "title": title,
                "role": "worker",
                "provider": "codex",
                "state": state,
            })
        };
        let mut report = serde_json::json!({
            "room": {
                "id": "room-nav",
                "participants": [
                    make_call("call-0", "att-0", "alpha", "working"),
                    make_call("call-1", "att-1", "beta", "blocked"),
                    make_call("call-2", "att-2", "gamma", "working"),
                    make_call("call-3", "att-3", "delta", "working"),
                    make_call("call-4", "att-4", "target", "working"),
                ]
            }
        });
        let page = 2;
        let mut nav = crate::navigation::Navigation::default();
        let mut fence = super::SelectionFence::None;
        let visible_now = |rep: &serde_json::Value, n: &crate::navigation::Navigation| {
            crate::render::visible(rep, n)
        };
        let send_key = |nav: &mut crate::navigation::Navigation,
                        fence: &mut super::SelectionFence,
                        rep: &serde_json::Value,
                        key: &Key| {
            let before = visible_now(rep, nav);
            super::apply_board_key(nav, fence, key, rep, &before, page)
        };

        let initial = visible_now(&report, &nav);
        let target_id = ("call-4".to_owned(), "att-4".to_owned());
        let target_idx = initial
            .iter()
            .position(|c| super::call_identity(c) == target_id)
            .unwrap();
        nav.selected = target_idx;
        nav.offset = target_idx.saturating_sub(1);

        // 1. Esc after editing a filter restores the prior call by identity even if
        //    a live refresh shifts rows while editing, and clamps viewport offset.
        send_key(&mut nav, &mut fence, &report, &Key::Char('/'));
        send_key(&mut nav, &mut fence, &report, &Key::Char('z'));
        assert!(visible_now(&report, &nav).is_empty());

        let pre_edit_refresh = visible_now(&report, &nav);
        report["room"]["participants"] = serde_json::json!([
            make_call("call-1", "att-1", "beta", "blocked"),
            make_call("call-4", "att-4", "target", "working"),
            make_call("call-3", "att-3", "delta", "working"),
        ]);
        let post_edit_refresh = visible_now(&report, &nav);
        super::reconcile_selection_with_page(
            &pre_edit_refresh,
            &post_edit_refresh,
            &mut nav,
            &mut fence,
            page,
        );

        send_key(&mut nav, &mut fence, &report, &Key::Escape);
        let after_esc = visible_now(&report, &nav);
        assert!(!nav.editing && nav.filter.is_empty());
        assert_eq!(
            super::call_identity(&after_esc[nav.selected]),
            target_id,
            "Esc after filter edit must restore prior call"
        );
        assert!(nav.offset <= after_esc.len().saturating_sub(page));
        assert!(nav.selected >= nav.offset && nav.selected < nav.offset + page);

        // 2. `b` toggle enters attention-only (falling back to row 0 when the selected call
        //    is not an attention call) and leaving `b` keeps the call selected in attention mode
        //    even if rows reordered while in attention-only.
        let blocked_id = ("call-1".to_owned(), "att-1".to_owned());
        send_key(&mut nav, &mut fence, &report, &Key::Char('b'));
        assert!(nav.attention_only);
        assert_eq!((nav.selected, nav.offset), (0, 0));

        let pre_att_refresh = visible_now(&report, &nav);
        report["room"]["participants"] = serde_json::json!([
            make_call("call-3", "att-3", "delta", "working"),
            make_call("call-0", "att-0", "alpha", "working"),
            make_call("call-1", "att-1", "beta", "blocked"),
            make_call("call-4", "att-4", "target", "working"),
        ]);
        let post_att_refresh = visible_now(&report, &nav);
        super::reconcile_selection_with_page(
            &pre_att_refresh,
            &post_att_refresh,
            &mut nav,
            &mut fence,
            page,
        );
        assert!(fence == super::SelectionFence::None);

        send_key(&mut nav, &mut fence, &report, &Key::Char('b'));
        assert!(!nav.attention_only);
        let after_b = visible_now(&report, &nav);
        assert_eq!(
            super::call_identity(&after_b[nav.selected]),
            blocked_id,
            "leaving attention-only must keep the currently selected attention call"
        );
        assert!(nav.offset <= after_b.len().saturating_sub(page));
        assert!(nav.selected >= nav.offset && nav.selected < nav.offset + page);

        // 2b. Enter attention-only with a visible blocked call selected: it stays selected
        //     (reconciled by call identity with clamped offset), not reset to row 0.
        let multi_report = serde_json::json!({
            "room": {
                "id": "room-nav",
                "participants": [
                    make_call("call-0", "att-0", "alpha", "working"),
                    make_call("call-1", "att-1", "beta", "blocked"),
                    make_call("call-3", "att-3", "delta", "working"),
                    make_call("call-2", "att-2", "gamma", "blocked"),
                    make_call("call-4", "att-4", "target", "working"),
                ]
            }
        });
        let blocked2_id = ("call-2".to_owned(), "att-2".to_owned());
        let mut multi_nav = crate::navigation::Navigation::default();
        let mut multi_fence = super::SelectionFence::None;
        let full_multi = visible_now(&multi_report, &multi_nav);
        let blocked2_idx = full_multi
            .iter()
            .position(|c| super::call_identity(c) == blocked2_id)
            .unwrap();
        multi_nav.selected = blocked2_idx;
        multi_nav.offset = blocked2_idx.saturating_sub(1);
        send_key(
            &mut multi_nav,
            &mut multi_fence,
            &multi_report,
            &Key::Char('b'),
        );
        assert!(multi_nav.attention_only);
        let att_multi = visible_now(&multi_report, &multi_nav);
        assert_eq!(att_multi.len(), 2);
        assert_eq!(
            super::call_identity(&att_multi[multi_nav.selected]),
            blocked2_id,
            "entering attention-only with a visible blocked call selected must keep it selected"
        );
        assert_ne!(
            multi_nav.selected, 0,
            "fixture should place blocked call after row 0"
        );
        assert!(multi_nav.offset <= att_multi.len().saturating_sub(page));
        assert!(
            multi_nav.selected >= multi_nav.offset && multi_nav.selected < multi_nav.offset + page
        );
        send_key(
            &mut multi_nav,
            &mut multi_fence,
            &multi_report,
            &Key::Char('b'),
        );
        assert!(!multi_nav.attention_only);

        // 2c. Enter attention-only from a non-attention call, move within attention to call X,
        //     then press `b`: X stays selected (not the stale pre-attention snapshot).
        let full_before_move = visible_now(&multi_report, &multi_nav);
        let target_full_idx = full_before_move
            .iter()
            .position(|c| super::call_identity(c) == target_id)
            .unwrap();
        multi_nav.selected = target_full_idx;
        multi_nav.offset = target_full_idx.saturating_sub(1);
        send_key(
            &mut multi_nav,
            &mut multi_fence,
            &multi_report,
            &Key::Char('b'),
        );
        assert!(multi_nav.attention_only);
        assert_eq!((multi_nav.selected, multi_nav.offset), (0, 0));
        send_key(&mut multi_nav, &mut multi_fence, &multi_report, &Key::Down);
        let att_moved = visible_now(&multi_report, &multi_nav);
        let moved_id = super::call_identity(&att_moved[multi_nav.selected]);
        assert_eq!(moved_id, blocked2_id);
        send_key(
            &mut multi_nav,
            &mut multi_fence,
            &multi_report,
            &Key::Char('b'),
        );
        assert!(!multi_nav.attention_only);
        let after_move_leave = visible_now(&multi_report, &multi_nav);
        assert_eq!(
            super::call_identity(&after_move_leave[multi_nav.selected]),
            moved_id,
            "leaving attention-only after moving must keep the selected attention call"
        );
        assert!(multi_nav.offset <= after_move_leave.len().saturating_sub(page));
        assert!(
            multi_nav.selected >= multi_nav.offset && multi_nav.selected < multi_nav.offset + page
        );

        // 2d. Enter attention-only with a non-attention call selected when attention list is
        //     empty: row 0. Leave without moving: the original call is restored.
        let empty_att_report = serde_json::json!({
            "room": {
                "id": "room-nav",
                "participants": [
                    make_call("call-0", "att-0", "alpha", "working"),
                    make_call("call-3", "att-3", "delta", "working"),
                    make_call("call-4", "att-4", "target", "working"),
                ]
            }
        });
        let mut empty_nav = crate::navigation::Navigation::default();
        let mut empty_fence = super::SelectionFence::None;
        let full_no_att = visible_now(&empty_att_report, &empty_nav);
        let orig_idx = full_no_att
            .iter()
            .position(|c| super::call_identity(c) == target_id)
            .unwrap();
        empty_nav.selected = orig_idx;
        empty_nav.offset = orig_idx.saturating_sub(1);
        send_key(
            &mut empty_nav,
            &mut empty_fence,
            &empty_att_report,
            &Key::Char('b'),
        );
        assert!(empty_nav.attention_only);
        assert!(visible_now(&empty_att_report, &empty_nav).is_empty());
        assert_eq!((empty_nav.selected, empty_nav.offset), (0, 0));
        send_key(
            &mut empty_nav,
            &mut empty_fence,
            &empty_att_report,
            &Key::Char('b'),
        );
        assert!(!empty_nav.attention_only);
        let restored = visible_now(&empty_att_report, &empty_nav);
        assert_eq!(
            super::call_identity(&restored[empty_nav.selected]),
            target_id,
            "leaving empty attention-only without moving must restore the original call"
        );
        assert!(empty_nav.offset <= restored.len().saturating_sub(page));
        assert!(
            empty_nav.selected >= empty_nav.offset && empty_nav.selected < empty_nav.offset + page
        );

        // 2e. Selection fence is preserved across `b` toggles so Enter never opens a
        //     different call while fenced.
        let mut fenced_nav = crate::navigation::Navigation::default();
        let mut active_fence = super::SelectionFence::Changed;
        send_key(&mut fenced_nav, &mut active_fence, &report, &Key::Char('b'));
        assert!(fenced_nav.attention_only);
        assert!(active_fence == super::SelectionFence::Changed);
        assert_eq!(
            send_key(&mut fenced_nav, &mut active_fence, &report, &Key::Enter),
            crate::navigation::Action::None
        );
        assert!(active_fence == super::SelectionFence::None);
        assert!(!fenced_nav.detail);

        // Reselect target_id before testing live refresh reorder below.
        let before_step3 = visible_now(&report, &nav);
        let step3_idx = before_step3
            .iter()
            .position(|c| super::call_identity(c) == target_id)
            .unwrap();
        nav.selected = step3_idx;
        nav.offset = step3_idx.saturating_sub(1);

        // 3. A live refresh that reorders rows keeps the same call selected, updates
        //    viewport offset so the row stays visible, and does not trip the fence.
        nav.detail = true;
        let before_reorder = visible_now(&report, &nav);
        let selected_before = super::call_identity(&before_reorder[nav.selected]);
        assert_eq!(selected_before, target_id);
        let old_index = nav.selected;
        report["room"]["participants"] = serde_json::json!([
            make_call("call-4", "att-4", "target", "working"),
            make_call("call-3", "att-3", "delta", "working"),
            make_call("call-2", "att-2", "gamma", "working"),
            make_call("call-1", "att-1", "beta", "blocked"),
            make_call("call-0", "att-0", "alpha", "working"),
        ]);
        let after_reorder = visible_now(&report, &nav);
        super::reconcile_selection_with_page(
            &before_reorder,
            &after_reorder,
            &mut nav,
            &mut fence,
            page,
        );
        assert_ne!(nav.selected, old_index, "fixture should actually move row");
        assert_eq!(
            super::call_identity(&after_reorder[nav.selected]),
            target_id,
            "refresh reorder must keep the same call selected"
        );
        assert!(nav.selected >= nav.offset && nav.selected < nav.offset + page);
        assert!(fence == super::SelectionFence::None);
        assert!(nav.detail);

        // 4. Starting in attention-only and pressing `b` preserves the selected call.
        let mut started_att = crate::navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let mut att_fence = super::SelectionFence::None;
        send_key(&mut started_att, &mut att_fence, &report, &Key::Char('b'));
        assert!(!started_att.attention_only);
        let full = visible_now(&report, &started_att);
        assert_eq!(
            super::call_identity(&full[started_att.selected]),
            ("call-1".to_owned(), "att-1".to_owned())
        );
        assert!(
            started_att.selected >= started_att.offset
                && started_att.selected < started_att.offset + page
        );
    }

    #[test]
    fn row_dock_renders_synthetic_attention_rows_without_worker_result_fallback() {
        let now = 1_800_000_600u64;
        let report = serde_json::json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-attn-dock",
                "owner": "main-1",
                "participants": [
                    {"participant": "main-1", "role": "main", "provider": "codex", "state": "working", "joined": true},
                    {"participant": "w-ended", "role": "worker", "provider": "codex", "state": "done", "joined": true, "task_id": "t-held", "updated_at": now - 400, "refs": {"plan_id": "plan-b"}},
                    {"participant": "w-old-plan", "role": "worker", "provider": "codex", "state": "done", "joined": true, "task_id": "t-held", "updated_at": now - 800, "refs": {"plan_id": "plan-old"}}
                ],
                "messages": [
                    {
                        "id": "msg-q1",
                        "message_kind": "question",
                        "sender": "main-1",
                        "text": "Approve release scope before tagging v1.2 with full migration notes?",
                        "pending_for": ["user"],
                        "answered": false,
                        "ts": now - 180
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-b",
                "goal": "Check attention dock",
                "tasks": {
                    "t-held": {
                        "id": "t-held",
                        "state": "running",
                        "title": "Run final checks",
                        "claimed_by_participant": "main-1",
                        "claimed_at": now - 600
                    }
                }
            }
        });
        let nav = crate::navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let rows = crate::render::visible(&report, &nav);
        let q_row = rows
            .iter()
            .find(|r| r["participant"] == "message:msg-q1")
            .expect("synthetic message row");
        let t_row = rows
            .iter()
            .find(|r| r["participant"] == "task:t-held")
            .expect("synthetic task row");
        let spy = crate::orchestrator::codex_chat_fixture::BoardSpy::new("attn-dock");
        let stop = std::sync::atomic::AtomicBool::new(false);

        let q_dock =
            super::row_dock(std::path::Path::new("."), &report, q_row, &spy, &stop).unwrap();
        assert!(q_dock.title.contains("Room question"), "{}", q_dock.title);
        assert!(
            q_dock
                .history
                .contains("Approve release scope before tagging v1.2 with full migration notes?"),
            "{}",
            q_dock.history
        );
        assert!(
            !q_dock
                .history
                .contains("No result recorded for this worker yet"),
            "{}",
            q_dock.history
        );

        let t_dock =
            super::row_dock(std::path::Path::new("."), &report, t_row, &spy, &stop).unwrap();
        assert_eq!(t_dock.title, "Task t-held");
        assert!(
            t_dock.history.contains("state: running"),
            "{}",
            t_dock.history
        );
        assert!(
            t_dock.history.contains("workers ended, still held"),
            "{}",
            t_dock.history
        );
        assert!(
            t_dock.history.contains("w-ended (done)"),
            "{}",
            t_dock.history
        );
        assert!(
            !t_dock.history.contains("w-old-plan"),
            "held-task dock leaked worker from another plan: {}",
            t_dock.history
        );
        assert!(
            !t_dock
                .history
                .contains("No result recorded for this worker yet"),
            "{}",
            t_dock.history
        );
    }

    #[test]
    fn opening_attention_only_rows_marks_message_held_task_and_worker_call_read() {
        struct TempRepo(std::path::PathBuf);
        impl Drop for TempRepo {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-watch-attn-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".oms")).unwrap();
        let repo = TempRepo(dir);

        let now = 1_800_000_600u64;
        let report = serde_json::json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-attn-read",
                "owner": "main-1",
                "participants": [
                    {"participant": "main-1", "role": "main", "provider": "codex", "state": "working", "joined": true},
                    {"participant": "w-fail", "role": "worker", "provider": "claude", "state": "failed", "joined": true, "task_id": "t-fail", "updated_at": now - 300},
                    {"participant": "w-ended", "role": "worker", "provider": "codex", "state": "done", "joined": true, "task_id": "t-held", "updated_at": now - 400, "refs": {"plan_id": "plan-r"}}
                ],
                "messages": [
                    {
                        "id": "msg-q1",
                        "message_kind": "question",
                        "sender": "main-1",
                        "text": "Approve release scope?",
                        "pending_for": ["user"],
                        "answered": false,
                        "ts": now - 180
                    }
                ]
            },
            "plan": {
                "plan_id": "plan-r",
                "goal": "Mark opened attention rows read",
                "tasks": {
                    "t-held": {"id": "t-held", "state": "running", "claimed_at": now - 600}
                }
            }
        });
        let mut nav = crate::navigation::Navigation {
            attention_only: true,
            ..Default::default()
        };
        let rows = crate::render::visible(&report, &nav);
        let q_row = rows
            .iter()
            .find(|r| r["participant"] == "message:msg-q1")
            .expect("synthetic message row");
        let t_row = rows
            .iter()
            .find(|r| r["participant"] == "task:t-held")
            .expect("synthetic task row");
        let c_row = rows
            .iter()
            .find(|r| r["participant"] == "w-fail")
            .expect("worker call row");

        let mut read = crate::attention::load(&repo.0, "room-attn-read");
        let unread_for =
            |read: &std::collections::BTreeMap<String, String>, kind: &str, id: &str| {
                crate::attention::items(&report, read)
                    .into_iter()
                    .find(|item| item.row_id == (kind.to_owned(), id.to_owned()))
                    .expect("attention item present")
                    .unread
            };
        assert!(unread_for(&read, "message", "msg-q1"));
        assert!(unread_for(&read, "task", "t-held"));
        assert!(unread_for(&read, "call", "w-fail"));

        super::mark_opened_row_read(
            &repo.0,
            "room-attn-read",
            &report,
            q_row,
            &mut read,
            &mut nav,
        );
        assert!(nav.notice.is_none(), "{:?}", nav.notice);
        assert!(!unread_for(&read, "message", "msg-q1"));
        assert!(unread_for(&read, "task", "t-held"));
        assert!(unread_for(&read, "call", "w-fail"));

        super::mark_opened_row_read(
            &repo.0,
            "room-attn-read",
            &report,
            t_row,
            &mut read,
            &mut nav,
        );
        assert!(nav.notice.is_none(), "{:?}", nav.notice);
        assert!(!unread_for(&read, "message", "msg-q1"));
        assert!(!unread_for(&read, "task", "t-held"));
        assert!(unread_for(&read, "call", "w-fail"));

        super::mark_opened_row_read(
            &repo.0,
            "room-attn-read",
            &report,
            c_row,
            &mut read,
            &mut nav,
        );
        assert!(nav.notice.is_none(), "{:?}", nav.notice);
        assert!(!unread_for(&read, "message", "msg-q1"));
        assert!(!unread_for(&read, "task", "t-held"));
        assert!(!unread_for(&read, "call", "w-fail"));
    }

    #[test]
    fn open_login_window_uses_tmux_argv_or_reports_fallback() {
        use crate::{Error, Result, Services, commands};
        use oms_panel_core::{
            NativeCommand,
            services::{CommandOutput, ExecutionRequest, PeerExecutor},
        };
        use std::path::Path;
        use std::sync::atomic::AtomicBool;
        struct LoginTmux {
            calls: std::sync::Mutex<Vec<Vec<std::ffi::OsString>>>,
            session_reply: Option<&'static str>,
            fail_new_window: bool,
        }
        impl PeerExecutor for LoginTmux {
            fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                self.calls.lock().unwrap().push(request.argv.clone());
                if request.argv.get(1).and_then(|v| v.to_str()) == Some("display-message") {
                    return match self.session_reply {
                        Some(id) => Ok(CommandOutput {
                            stdout: format!("{id}\n").into_bytes(),
                            cleanup_complete: true,
                            output_complete: true,
                            ..Default::default()
                        }),
                        None => Err(Error::new("no tmux client")),
                    };
                }
                if self.fail_new_window {
                    return Err(Error::new("tmux failed"));
                }
                Ok(CommandOutput {
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }
        impl Services for LoginTmux {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[std::ffi::OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                panic!("unexpected native command {command:?}")
            }
        }

        let stop = AtomicBool::new(false);
        let repo = Path::new("repo-fixture");
        let source = Path::new("");

        // Direct session hint spawns `tmux new-window -t $3: -n login -c <repo> <oms...> connect-services --login-menu`.
        let tmux = LoginTmux {
            calls: std::sync::Mutex::new(Vec::new()),
            session_reply: None,
            fail_new_window: false,
        };
        let notice = super::open_login_window(repo, source, Some("$3"), None, &tmux, &stop);
        assert_eq!(notice, "Sign-in window opened");
        let calls = tmux.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let argv: Vec<String> = calls[0]
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &argv[..8],
            [
                "tmux",
                "new-window",
                "-t",
                "$3:",
                "-n",
                "login",
                "-c",
                "repo-fixture"
            ]
        );
        assert_eq!(
            &argv[argv.len() - 2..],
            ["connect-services", "--login-menu"]
        );

        // Falls back to querying session from TMUX_PANE when no session hint is given.
        let tmux = LoginTmux {
            calls: std::sync::Mutex::new(Vec::new()),
            session_reply: Some("$7"),
            fail_new_window: false,
        };
        let notice = super::open_login_window(repo, source, None, Some("%5"), &tmux, &stop);
        assert_eq!(notice, "Sign-in window opened");
        let calls = tmux.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            commands::strings(&["tmux", "display-message", "-p", "-t", "%5", "#{session_id}"])
        );
        assert_eq!(calls[1][3], "$7:");

        // Without any tmux session, returns fallback notice naming the command.
        let tmux = LoginTmux {
            calls: std::sync::Mutex::new(Vec::new()),
            session_reply: None,
            fail_new_window: false,
        };
        let notice = super::open_login_window(repo, source, None, None, &tmux, &stop);
        assert!(
            notice.contains("oms connect-services --login-menu"),
            "{notice}"
        );
        assert!(tmux.calls.lock().unwrap().is_empty());

        // When tmux new-window fails, returns notice naming the fallback command.
        let tmux = LoginTmux {
            calls: std::sync::Mutex::new(Vec::new()),
            session_reply: None,
            fail_new_window: true,
        };
        let notice = super::open_login_window(repo, source, Some("$3"), None, &tmux, &stop);
        assert!(
            notice.contains("oms connect-services --login-menu"),
            "{notice}"
        );
    }
}
