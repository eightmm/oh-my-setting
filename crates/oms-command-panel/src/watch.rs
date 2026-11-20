use crate::{
    Error, Output, Result, Services, attention,
    board_refresh::{BoardRefresh, ChatReads, RefreshKey},
    cli::Options,
    commands, input, navigation, render,
};
use oms_panel_core::{NativeCommand, Value, eventlog, state::Snapshot};
use serde_json::json;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub fn run(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Result<Output> {
    let mut output = std::io::stdout().lock();
    run_with_writer(repo, options, services, interrupted, &mut output)
}
/// Watch output is streamed rather than retained in an unbounded command result.
pub fn run_with_writer(
    repo: &Path,
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
        .then(InteractiveRefresh::start)
        .transpose()?;
    if board.is_some() {
        room.clear();
    }
    let mut requested = true;
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
    let mut weekly = WeeklyUsage::default();
    let mut chat: Option<ChatDock> = None;
    let mut split = DockSplit::default();
    let mut observations = crate::orchestrator::Observations::default();
    let mut main_footer = MainFooter::default();
    let mut selection_changed = SelectionFence::None;
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
        let (mut report, snapshot_ok) = if let Some(board) = &mut board {
            let (live, ready) = scope_sample.take().unwrap_or_else(|| {
                let ready = board.worker.ready();
                (target.sample(repo, services, interrupted), ready)
            });
            let checkout_changed = board.checkout_changed(&live);
            let launch = std::mem::take(&mut requested)
                || (Instant::now() >= deadline && !board.worker.busy());
            let (changed, result) = board.update(live, launch, ready);
            accepted_refresh = result.is_some() && board.phase == RefreshPhase::Fresh;
            if checkout_changed && let Some(Value::Object(fields)) = &mut previous {
                fields.remove("board_checkout");
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
                nav.selected = 0;
                nav.offset = 0;
                nav.detail = false;
                nav.folded.clear();
                nav.notice = None;
            }
            (
                board.phase.frame_report(result, previous.take(), &room),
                board.phase == RefreshPhase::Fresh,
            )
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
            main_footer.project(
                board.scope.as_ref(),
                accepted_refresh,
                &mut report,
                Instant::now(),
                |scope| target.read_footer(repo, services, interrupted, scope),
            );
        }
        let pending = report["collection"]["pending"] == true;
        if interactive
            && board.as_ref().is_none_or(|board| board.scope.is_some())
            && let Ok(panel) = crate::orchestrator::Panel::load(repo, &room, services, interrupted)
        {
            if !pending && (board.is_none() || snapshot_ok) {
                panel.observe(&mut observations, &mut report, snapshot_ok);
            }
            if !observations.quota_pending()
                && nav
                    .notice
                    .as_deref()
                    .is_some_and(|s| s.starts_with(QUOTA_CHECKING))
            {
                nav.notice = None;
            }
            report["native_dock"] = serde_json::json!(panel.docked());
        } else {
            if board.is_some() {
                report["native_dock"] = Value::Null;
            }
            if !pending {
                observations.clear_chats();
            }
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
            reconcile_selection(&before, &after, &mut nav, &mut selection_changed);
        }
        if let Some((participant, binding)) = selected_chat
            && observations.chat_binding(&participant) != binding.as_ref()
        {
            selection_changed = SelectionFence::Native;
            nav.detail = false;
        }
        let owner = board
            .as_ref()
            .and_then(|board| board.scope.as_ref())
            .and_then(|scope| scope.main.as_ref())
            .map(|main| main.owner.clone());
        let owner = owner.as_deref().or_else(|| options.get("owner"));
        if settle_live_chat_dock(
            &mut chat,
            &observations,
            &report,
            repo,
            &room,
            owner,
            board.as_ref(),
            session.as_ref(),
            services,
            interrupted,
        ) {
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
            let frame = frame(
                &report,
                &nav,
                size,
                &read,
                colored,
                interactive.then(|| split.rows(size.1)),
                chat.as_mut(),
                weekly.overlay(weekly_phase),
            );
            provider_rows = frame.provider_rows;
            worker_rows = frame.worker_rows;
            weekly_hit = frame.weekly_usage;
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
        let bounded_retry =
            observations.quota_pending() || chat.as_ref().is_some_and(|dock| dock.chat.is_some());
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
                let (keys, woken) = terminal.wait_woken(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(100)),
                    wake.as_ref(),
                )?;
                let mut refresh = woken;
                let mut collect_ready = false;
                let mut scope = InputScope {
                    current: board.as_ref().is_none_or(|board| board.scope.is_some()),
                    ..Default::default()
                };
                if let Some(board) = &mut board {
                    collect_ready = scope.current && board.worker.ready();
                    if collect_ready || woken {
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
                    if weekly.detail.is_none()
                        && !weekly.repaint
                        && report["native_dock"].is_null()
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
                        && pending
                        && matches!(key, input::Key::Escape)
                        && chat.as_ref().is_some_and(|dock| dock.chat.is_some())
                    {
                        chat = None;
                        continue;
                    }
                    if let Some(dock) = &mut chat
                        && !nav.editing
                        && (!pending || dock.chat.is_none())
                    {
                        match key {
                            input::Key::Escape | input::Key::Char('q') => {
                                chat = None;
                                continue;
                            }
                            _ if dock
                                .scroll(&key, dock_page(split.rows(last_size.1), last_size.0)) =>
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
                            &calls,
                            id,
                            &mut nav,
                            &mut selection_changed,
                            row_authority,
                        ) {
                            match worker_dock(repo, call, services, interrupted) {
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
                    let identity = calls.get(nav.selected).map(call_identity);
                    let old_view = nav.view;
                    let page = key_page(
                        &report,
                        &nav,
                        last_size,
                        interactive.then(|| split.rows(last_size.1)),
                        &read,
                    );
                    let action = if let input::Key::Click(x, y) = key {
                        if crate::layout::dimensions() == last_size
                            && x > 0
                            && usize::from(x) <= last_size.0
                        {
                            provider_at_row(&provider_rows, y)
                                .map_or(navigation::Action::None, navigation::Action::Provider)
                        } else {
                            navigation::Action::None
                        }
                    } else if !action_scope_current && direct_detail {
                        deny_row_action(&mut nav);
                        navigation::Action::None
                    } else {
                        apply_selection_key(
                            &mut nav,
                            &mut selection_changed,
                            &key,
                            calls.len(),
                            page,
                        )
                    };
                    if nav.view != old_view {
                        let reordered = render::visible(&report, &nav);
                        if let Some(index) = identity.and_then(|id| {
                            reordered.iter().position(|call| call_identity(call) == id)
                        }) {
                            nav.selected = index;
                        }
                    }
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
                            let council = render::calls(&report)
                                .into_iter()
                                .filter(|call| call["tool"] == "panel-council")
                                .max_by(|a, b| {
                                    crate::text(a, "updated_at").cmp(crate::text(b, "updated_at"))
                                });
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
                                let items: Vec<_> = attention::items(&report, &read)
                                    .into_iter()
                                    .filter(|item| {
                                        item.row_id.0 == "call"
                                            && item.row_id.1 == crate::text(call, "participant")
                                    })
                                    .collect();
                                if let Err(e) = attention::mark(repo, &room, &report, &items) {
                                    nav.notice = Some(e.message);
                                }
                                read = attention::load(repo, &room);
                                if action == navigation::Action::Open && call["role"] != "main" {
                                    nav.detail = false;
                                    match worker_dock(repo, call, services, interrupted) {
                                        Ok(dock) => chat = Some(dock),
                                        Err(err) => nav.notice = Some(err.message),
                                    }
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
                if refresh || collect_ready || scope.changed {
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
                    let frame = frame(
                        &report,
                        &nav,
                        size,
                        &read,
                        colored,
                        interactive.then(|| split.rows(size.1)),
                        chat.as_mut(),
                        weekly.overlay(weekly_phase.awaiting(refresh || collect_ready)),
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
                    weekly_hit = frame.weekly_usage;
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
                if (changed
                    || weekly.repaint
                    || ((detail_repaint || scope.checkout_repaint) && !dock_moved))
                    && !scope.changed
                {
                    let frame = frame(
                        &report,
                        &nav,
                        last_size,
                        &read,
                        colored,
                        interactive.then(|| split.rows(last_size.1)),
                        chat.as_mut(),
                        weekly.overlay(weekly_phase.awaiting(refresh || collect_ready)),
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
                    weekly_hit = frame.weekly_usage;
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
                if refresh || collect_ready || scope.changed {
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
                    let frame = frame(
                        &report,
                        &nav,
                        size,
                        &read,
                        colored,
                        interactive.then(|| split.rows(size.1)),
                        chat.as_mut(),
                        weekly.overlay(weekly_phase),
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
                    weekly_hit = frame.weekly_usage;
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
const EVENT_BACKED_FALLBACK: Duration = Duration::from_secs(30);

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
        Key::Enter | Key::Char('1' | '2' | '3' | 'U' | 'w' | 'D' | 'c' | 'f' | 'r' | 'R' | 'h') => {
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
struct LiveMain {
    attempt: String,
    owner: String,
    session: String,
    window: String,
    pane: String,
    pid: u32,
    birth: String,
}

struct BoardTarget {
    room: Option<String>,
    caller: Option<String>,
    managed: bool,
}

const FOOTER_INTERVAL: Duration = Duration::from_secs(30);

struct FooterReading {
    scope: BoardScope,
    model: Option<String>,
    checked_at: Instant,
}

#[derive(Default)]
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
                        && row[9] == row[6]
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
                        && board[9] == board[6]
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
                .with_launch_path(launch_path.clone()),
                room: room.unwrap_or_default(),
                main,
            },
            launch_path,
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshPhase {
    Pending,
    Fresh,
    Failed(String),
}

fn clear_private_weekly(report: &mut Value) {
    if let Some(fields) = report.as_object_mut() {
        fields.remove("claude_local_weekly");
        fields.remove("codex_weekly_account");
    }
}

fn reset_weekly_scope(weekly: &mut WeeklyUsage, changed: bool) {
    if changed {
        *weekly = WeeklyUsage::default();
    }
}

impl RefreshPhase {
    fn frame_report(
        &self,
        snapshot: Option<Snapshot>,
        previous: Option<Value>,
        room: &str,
    ) -> Value {
        let mut report = if let Some(snapshot) = snapshot {
            let mut current = snapshot.report;
            // A display-only result has no pane reads of its own. Retain the
            // same-scope observations so its native rows keep their identity.
            if *self == Self::Pending {
                let mut previous = match previous {
                    Some(Value::Object(previous)) => previous,
                    _ => Default::default(),
                };
                for key in [
                    "provider_sessions",
                    "provider_models",
                    "provider_activity",
                    "provider_accounts",
                    "provider_status",
                    "native_children",
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
        match self {
            Self::Fresh => {}
            Self::Pending => {
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

struct InteractiveRefresh {
    worker: BoardRefresh,
    scope: Option<BoardScope>,
    phase: RefreshPhase,
    deferred_refresh: bool,
}

impl InteractiveRefresh {
    fn start() -> Result<Self> {
        Ok(Self {
            worker: BoardRefresh::start()?,
            scope: None,
            phase: RefreshPhase::Pending,
            deferred_refresh: false,
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

    fn request(&mut self, scope: &BoardScope) {
        self.phase = match self.worker.request(scope.key.clone()) {
            Ok(_) => RefreshPhase::Pending,
            Err(error) => RefreshPhase::Failed(error.message),
        };
    }

    fn update(
        &mut self,
        live: Result<BoardScope>,
        request: bool,
        ready: bool,
    ) -> (bool, Option<Snapshot>) {
        let changed = !self.matches(&live);
        if changed {
            self.deferred_refresh = false;
        }
        let scope = match live {
            Ok(scope) => scope,
            Err(error) => {
                self.scope = None;
                self.phase = RefreshPhase::Failed(error.message);
                return (changed, None);
            }
        };
        if changed || (request && !self.worker.busy()) {
            self.request(&scope);
        } else if request && !self.worker.queued() {
            // Keep the current scope's read eligible to paint even if wakes
            // arrive faster than it completes. An already queued scope change
            // covers wakes until its collection starts.
            self.deferred_refresh = true;
        }
        let result = ready.then(|| self.worker.poll(&scope.key)).flatten();
        let follow_up = !self.worker.busy() && std::mem::take(&mut self.deferred_refresh);
        if follow_up {
            self.request(&scope);
        }
        let completed = result.and_then(|result| match result {
            Ok(snapshot)
                if snapshot.status == 0
                    && snapshot.report["collection"]["ok"] == true
                    && scope.main.as_ref().is_none_or(|main| {
                        let attempt = &snapshot.report["room_attempts"][&main.attempt];
                        let member = crate::rows(&snapshot.report["room"]["participants"])
                            .iter()
                            .find(|row| row["participant"] == main.attempt);
                        attempt["provider"] == main.owner
                            && oms_panel_core::state::main_participant(attempt) == main.attempt
                            && member.is_some_and(|row| {
                                row["terminal"] != true
                                    && !row["state"].as_str().is_some_and(|state| {
                                        oms_panel_core::state::TERMINAL_STATES.contains(&state)
                                    })
                            })
                    }) =>
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
        self.scope = Some(scope);
        (changed, completed)
    }

    fn chat_cache(&self) -> Option<&oms_panel_core::state::AttemptCache> {
        if self.phase != RefreshPhase::Fresh {
            return None;
        }
        self.worker.idle_attempt_cache(&self.scope.as_ref()?.key)
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
        | Action::Results => row_authority,
        Action::None | Action::Quit | Action::Refresh | Action::Controls => true,
    };
    if allowed {
        action
    } else {
        deny_row_action(nav);
        Action::None
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
        if report["collection"]["ok"] != true || report["capacity_stale"] == true {
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
    } else if call["role"] == "main" {
        *fence = SelectionFence::None;
        nav.detail = true;
    } else {
        return Some(call);
    }
    None
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
    let title = crate::text(call, "title").trim();
    let title = if title.is_empty() {
        crate::text(call, "participant")
    } else {
        title
    };
    format!("{title} · {}", crate::text(call, "role"))
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
        history = "No result recorded for this worker yet".into();
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

    /// A gesture starts on the painted divider. A concurrent terminal resize
    /// invalidates the coordinates until the next mouse press.
    fn mouse(
        &mut self,
        key: &input::Key,
        painted: (usize, usize),
        actual: (usize, usize),
    ) -> Option<bool> {
        use input::Key;
        let (columns, height) = actual;
        let rows = self.rows(height);
        match *key {
            Key::Click(x, y) => {
                self.drag_size = None;
                if painted == actual
                    && rows != 0
                    && x > 0
                    && usize::from(x) <= columns
                    && usize::from(y) == height - rows + 1
                {
                    self.drag_size = Some(actual);
                    return Some(false);
                }
                None
            }
            Key::Drag(y) | Key::Release(y) => {
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
    dock_rows: Option<usize>,
    chat: Option<&mut ChatDock>,
    weekly: Option<(&mut ChatDock, WeeklyPhase)>,
) -> render::BoardFrame {
    let (columns, height) = size;
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
        });
        let lines = if height >= 3 {
            let mut lines = dock_lines(
                columns,
                height,
                colored,
                Some(placeholder.as_mut().unwrap_or(weekly)),
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
        };
    }
    let (nav, board_height) = board_viewport(report, nav, height, dock_rows);
    let nav = nav.as_ref();
    let chat = chat.filter(|dock| !private_chat_hidden(report, dock));
    let interactive = dock_rows.is_some();
    if interactive && let Some(provider) = report["native_dock"].as_str() {
        let mut board = render::board_frame(
            report,
            nav,
            columns,
            board_height,
            read,
            colored,
            interactive,
        );
        board.lines.resize(board_height, String::new());
        board.lines.push(render::native_dock_bar(
            provider,
            CLOSE_NATIVE,
            columns,
            colored,
        ));
        return board;
    }
    if board_height == height {
        return render::board_frame(report, nav, columns, height, read, colored, interactive);
    }
    let mut board = render::board_frame(
        report,
        nav,
        columns,
        board_height,
        read,
        colored,
        interactive,
    );
    let top = board_height;
    board.lines.resize(top, String::new());
    for hit in &mut board.provider_rows {
        if hit.is_some_and(|row| row >= top) {
            *hit = None;
        }
    }
    board.worker_rows.retain(|row, _| *row < top);
    board.weekly_usage = board.weekly_usage.filter(|hit| hit.row < top);
    board
        .lines
        .extend(dock_lines(columns, height - top, colored, chat));
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
    chat: Option<&mut ChatDock>,
) -> Vec<String> {
    let boxed = columns >= 44 && dock >= 4;
    let inner = if boxed { columns - 4 } else { columns };
    let page = dock_page(dock, columns);
    let (title, visible, hint) = if let Some(chat) = chat {
        let lines = history_rows(&chat.history, inner.max(1), colored);
        let latest = lines.len().saturating_sub(page);
        chat.back = chat.back.min(latest);
        (
            chat.title.as_str(),
            lines
                .into_iter()
                .skip(latest - chat.back)
                .take(page)
                .collect::<Vec<_>>(),
            "↑↓ Scroll · Esc Close",
        )
    } else {
        (
            "Details",
            vec![render::clip(
                "Select an orchestrator, worker, or council",
                inner,
            )],
            "",
        )
    };
    let mut lines = vec![render::dock_top(title, columns, colored)];
    let mut visible = visible;
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
    lines.push(render::clip(hint, columns));
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

fn reconcile_selection(
    before: &[Value],
    after: &[Value],
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
) {
    if let Some(selected) = before.get(nav.selected)
        && !after
            .iter()
            .any(|call| call_identity(call) == call_identity(selected))
    {
        if native_chat_child(selected) {
            *changed = SelectionFence::Native;
        } else if *changed == SelectionFence::None {
            *changed = SelectionFence::Changed;
        }
        nav.detail = false;
    }
    nav.selected = preserve_selection(before, after, nav.selected);
}

fn apply_selection_key(
    nav: &mut navigation::Navigation,
    changed: &mut SelectionFence,
    key: &input::Key,
    count: usize,
    page: usize,
) -> navigation::Action {
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
        return navigation::Action::None;
    }
    if direct_detail {
        nav.detail = count > 0;
        if nav.notice.as_deref() == Some(RETRY_DETAIL) {
            nav.notice = None;
        }
        return navigation::Action::None;
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
    nav.apply(key, count, page)
}

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
    use super::{draw, history_rows, provider_at_row};
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
            Some(12),
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
            Some(12),
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
        for (report, nav, size, dock) in [
            (
                report(Some("claude"), None),
                Navigation::default(),
                (100, 36),
                Some(12),
            ),
            (
                report(Some("claude"), None),
                detail.clone(),
                (100, 40),
                Some(12),
            ),
            (
                report(Some("claude"), None),
                Navigation::default(),
                (100, 12),
                Some(0),
            ),
            (report(None, None), detail, (100, 36), Some(12)),
            (
                report(None, None),
                Navigation::default(),
                (100, 12),
                Some(0),
            ),
            (
                report(None, Some("boom")),
                Navigation::default(),
                (100, 36),
                Some(12),
            ),
            (report(None, Some("boom")), noticed, (100, 30), Some(0)),
        ] {
            let painted = super::frame(&report, &nav, size, &read, false, dock, None, None);
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
                Some(4),
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
    fn weekly_usage_fits_above_the_default_details_dock_at_thirteen_rows() {
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
        let split = DockSplit::default();
        for height in [12, 13] {
            let size = (80, height);
            let dock = split.rows(height);
            assert_eq!(dock, 4);
            let divider = height - dock;
            let board = frame(
                &report,
                &nav,
                size,
                &Default::default(),
                true,
                Some(dock),
                None,
                None,
            );
            assert_eq!(board.lines.len(), height);
            assert!(board.lines[divider].contains("Details"));
            assert!(board.lines[divider - 1].contains("Enter"));
            assert!(
                board.lines[..divider]
                    .iter()
                    .all(|line| !line.contains("proof-tree"))
            );
            for text in ["Filter /Review", "First", "Goal: Keep Board usable"] {
                assert!(
                    board.lines[..divider]
                        .iter()
                        .any(|line| line.contains(text)),
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
            assert!(row < divider - 1 && board.lines[row].contains("Review terminal layout"));

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
            let picked =
                super::select_painted_call(&calls, worker_hit, &mut focus, &mut fence, true);
            assert_eq!(picked.map(super::call_identity), Some(worker_hit.clone()));
            assert!(!focus.detail);
            fence = super::SelectionFence::Native;
            assert!(
                super::select_painted_call(&calls, main_hit, &mut focus, &mut fence, false)
                    .is_none()
            );
            assert!(!focus.detail);
            assert!(
                super::select_painted_call(&calls, main_hit, &mut focus, &mut fence, true)
                    .is_none()
            );
            assert!(focus.detail && fence == super::SelectionFence::None);
            assert_eq!(super::call_identity(&calls[focus.selected]), *main_hit);

            if height == 12 {
                assert!(board.weekly_usage.is_none());
                assert!(board.lines.iter().all(|line| !line.contains("Weekly left")));
                continue;
            }
            let hit = board
                .weekly_usage
                .expect("Weekly fits above Details at 80x13");
            assert!(
                board.lines[..divider]
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
                    Some(dock),
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
        let dock = split.rows(13);
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
            Some(dock),
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
                Some(dock),
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
                Some(dock),
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
                Some(dock),
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
            scope: None,
            phase: RefreshPhase::Pending,
            deferred_refresh: false,
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
            Some(dock),
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
            Some(dock),
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
            Some(dock),
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
    fn divider_keeps_its_row_when_switching_content_at_short_heights() {
        let report = serde_json::json!({"collection": {"ok": true}});
        let nav = Default::default();
        let read = Default::default();
        let mut chat = super::ChatDock {
            title: "AGY · Recent chat".into(),
            history: "\x1b[31m한글 reply\x1b[0m ".repeat(40),
            back: 0,
            chat: None,
            agy_locator: None,
        };
        let split = super::DockSplit::default();
        for size @ (columns, height) in [(38, 12), (80, 17), (80, 23), (140, 90)] {
            let rows = split.rows(height);
            let divider = height - rows;
            let idle = super::frame(&report, &nav, size, &read, true, Some(rows), None, None);
            let opened = super::frame(
                &report,
                &nav,
                size,
                &read,
                true,
                Some(rows),
                Some(&mut chat),
                None,
            );
            assert_eq!(&idle.lines[..divider], &opened.lines[..divider]);
            assert!(idle.lines[divider].contains("Details"));
            assert!(opened.lines[divider].contains("AGY · Recent chat"));
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
        assert_eq!(split.mouse(&Key::Drag(18), size, size), Some(false));
        assert_eq!(split.mouse(&Key::Click(6, 24), size, size), None);
        assert_eq!(split.mouse(&Key::Click(6, 25), size, size), Some(false));
        assert_eq!(split.mouse(&Key::Drag(18), size, size), Some(true));
        assert_eq!(split.rows(36), 19);
        assert_eq!(split.mouse(&Key::Drag(1), size, size), Some(true));
        assert_eq!(split.rows(36), 20);
        assert_eq!(split.mouse(&Key::Release(36), size, size), Some(true));
        assert_eq!(split.rows(36), 4);
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
            Some(12),
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
        use super::{RefreshPhase, clear_private_weekly, idle_report};
        use oms_panel_core::state::Snapshot;
        use serde_json::json;

        let mut previous = idle_report("room-fixture");
        previous["provider_status"] = json!({"codex":{"weekly_used":14}});
        previous["provider_accounts"] =
            json!({"codex":{"account":"acct-b","pinned":false,"verification":"verified"}});
        previous["claude_local_weekly"] = json!({"weekly_used":14});
        previous["codex_weekly_account"] = json!("acct-b");

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
            assert_eq!(report["provider_status"]["codex"]["weekly_used"], 14);
            assert_eq!(report["provider_accounts"]["codex"]["account"], "acct-b");
        }
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
    fn managed_refresh_scope_follows_live_tags_and_ignores_focus() {
        use super::*;
        use crate::orchestrator::codex_chat_fixture::BoardSpy;
        use oms_panel_core::{CommandOutput, PeerExecutor, services::ExecutionRequest};

        struct Tmux<'a> {
            repo: &'a Path,
            room: &'static str,
            attempt: &'static str,
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
                let owner = self.owner;
                let repo = self.repo.display();
                let tagged = u8::from(self.board_tagged);
                let board = format!(
                    "$1\t@1\t%2\t{pid}\t0\t{tagged}\t{main}\t%1\t{owner}\t{main}\t{room}\t{repo}\t."
                );
                let mut main = format!(
                    "$1\t@1\t%1\t{pid}\t0\t0\t{main}\t%1\t{owner}\t{main}\t{room}\t{repo}\t."
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
            main_first: false,
            board_tagged: true,
            launch_path: None,
        };
        let first = target.sample(repo, &tmux, &stop).unwrap();
        assert_eq!(first.room, "room-a");
        tmux.main_first = true;
        assert_eq!(target.sample(repo, &tmux, &stop).unwrap(), first);
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
            scope: None,
            phase: RefreshPhase::Pending,
            deferred_refresh: false,
        };
        let scope = |room: &str| BoardScope {
            key: RefreshKey::new(repo, Some(room), Some(MAIN)).unwrap(),
            room: room.into(),
            main: Some(LiveMain {
                attempt: MAIN.into(),
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
        let held = board.phase.report(fresh.clone());
        assert!(!render::calls(&held).is_empty());
        assert!(board.chat_cache().is_none());
        assert!(!settle_chat_dock(&mut dock, &observations, &held));
        let paint = |report: &Value, nav: &navigation::Navigation, dock: Option<&mut ChatDock>| {
            frame(
                report,
                nav,
                (120, 40),
                &Default::default(),
                false,
                Some(12),
                dock,
                None,
            )
            .lines
            .join("\n")
        };
        let pending_paint = paint(&held, &nav, dock.as_mut());
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
        assert_eq!(board.phase, RefreshPhase::Pending);
        let painted = paint(
            &board.phase.frame_report(failed, Some(retained), ROOM),
            &nav,
            dock.as_mut(),
        );
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
        let cold = board.phase.report(idle_report(new_room));
        assert_eq!(cold["room"]["id"], new_room);
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
            Some(12),
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
        let dock = super::DockSplit::default().rows(height);
        assert_eq!(dock, 4);
        let frame = super::frame(
            &report,
            &nav,
            (80, height),
            &Default::default(),
            false,
            Some(dock),
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
            Some(12),
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
    #[test]
    fn agy_watch_real_private_fake_tmux_click_enter_escape_and_fallback() {
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

        const CHILD_ENV: &str = "OMS_TEST_AGY_WATCH_PTY";
        const SEED_ENV: &str = "OMS_TEST_AGY_WATCH_SEED";
        const REPLY_ENV: &str = "OMS_TEST_WATCH_REPLY";
        const WORKER: &str = "wake-worker";
        const TEST_NAME: &str =
            "watch::tests::agy_watch_real_private_fake_tmux_click_enter_escape_and_fallback";

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
            let seed_out = PathBuf::from(std::env::var_os("OMS_TEST_SEED_OUT").unwrap());
            std::fs::write(&seed_out, format!("{room_id}\n{attempt_id}\n")).unwrap();
            return;
        }

        if std::env::var_os(CHILD_ENV).is_some() {
            let repo = PathBuf::from(std::env::var_os("OMS_TEST_REPO").unwrap());
            let counters_file = PathBuf::from(std::env::var_os("OMS_TEST_COUNTERS").unwrap());
            let options = Options::parse(&commands::strings(&["--watch"])).unwrap();
            let services = SubcommandServices::sibling().unwrap();
            let interrupted = AtomicBool::new(false);
            crate::agy_child_chat::reset_all_agy_counters();
            let result = super::run(&repo, &options, &services, &interrupted).unwrap();
            let (custody, scans, strict_reads) = crate::agy_child_chat::test_counters();
            let (legacy_reads, strict_check) = crate::agy_child_chat::agy_child_read_counts();
            let (leg_custody, leg_parent, leg_cand, leg_child) =
                crate::agy_child_chat::legacy_agy_read_index();
            assert_eq!(strict_reads, strict_check);
            assert_eq!(legacy_reads, leg_child);
            assert_eq!(
                crate::agy_child_chat::total_agy_child_reads(),
                legacy_reads + strict_reads
            );
            std::fs::write(
                &counters_file,
                format!(
                    "{custody},{scans},{strict_reads},{legacy_reads},{leg_custody},{leg_parent},{leg_cand},{leg_child}\n"
                ),
            )
            .unwrap();
            assert_eq!(result.0, 0);
            return;
        }

        struct Temp(PathBuf);
        impl Temp {
            fn new() -> Self {
                let p = std::env::temp_dir()
                    .canonicalize()
                    .unwrap()
                    .join(format!("oms-agy-watch-pty-{}", std::process::id()));
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

        let tmp = Temp::new();
        let repo = tmp.0.join("repo");
        let wt = tmp.0.join("repo-wt");
        let home = tmp.0.join("home");
        let state = tmp.0.join("state");
        let bin = tmp.0.join("bin");
        let log = tmp.0.join("tmux-calls.log");
        let counters = tmp.0.join("counters.txt");
        let gemini = home.join(".gemini");
        let app_root = gemini.join("antigravity-cli");
        let presence = app_root.join("presence");
        let brain = app_root.join("brain");
        for d in [
            &repo, &home, &state, &bin, &gemini, &app_root, &presence, &brain,
        ] {
            mkdir_p(d);
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
            cmd.args(["--exact", TEST_NAME, "--nocapture"])
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

        // Private fake tmux binary logging all calls; PATH holds ONLY <tmp>/bin (with real git symlinked).
        let git_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("git"))
            .find(|p| p.is_file())
            .unwrap();
        std::os::unix::fs::symlink(&git_bin, bin.join("git")).unwrap();
        let fake_tmux = bin.join("tmux");
        let fake_tmux_src = r##"#!/usr/bin/python3
import os, sys
args = sys.argv[1:]
log = os.environ.get("OMS_FAKE_TMUX_LOG")
if log:
    with open(log, "a", encoding="utf-8") as f:
        f.write(" ".join(args) + "\n")
ppid = os.getppid()
if args == [
    "list-panes",
    "-t",
    "%2",
    "-F",
    "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t#{@oms_panel_owner}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_panel_repo}\t.\t#{?#{m/r:[[:cntrl:]],#{pane_start_path}},,#{pane_start_path}}",
]:
    repo = os.environ["OMS_TEST_REPO"]
    room = os.environ["OMS_FAKE_ROOM"]
    attempt = os.environ["OMS_FAKE_ATTEMPT"]
    execution = os.environ["OMS_TEST_EXECUTION"]
    print(f"$1\t@1\t%2\t{ppid}\t0\t1\t{attempt}\t%1\tcodex\t{attempt}\t{room}\t{repo}\t.")
    print(f"$1\t@1\t%1\t{ppid}\t0\t0\t{attempt}\t%1\tcodex\t{attempt}\t{room}\t{repo}\t.\t{execution}")
elif args == [
    "list-panes",
    "-s",
    "-t",
    "%2",
    "-F",
    "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{window_active}\t#{@oms_panel_native_pane}\t#{@oms_panel_main_attempt}\t#{@oms_panel_owner}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_provider_session}\t#{@oms_provider_parent}\t#{pane_active}\t.#{@oms_panel_main_account_ready}_#{@oms_codex_chat_bound}_#{@oms_claude_quota_bound}",
]:
    room = os.environ["OMS_FAKE_ROOM"]
    attempt = os.environ["OMS_FAKE_ATTEMPT"]
    lead_pid = os.environ["OMS_FAKE_LEAD_PID"]
    print(f"$1\t@1\t%1\t{ppid}\t0\t1\t%1\t{attempt}\tcodex\t{attempt}\t{room}\t\t\t1\t.__")
    print(f"$1\t@1\t%2\t{ppid}\t0\t1\t%1\t{attempt}\tcodex\t{attempt}\t{room}\t\t\t0\t.__")
    print(f"$1\t@4\t%4\t{lead_pid}\t0\t0\t\t\t\t\t{room}\tantigravity\t{attempt}\t0\t.__")
elif args == [
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
]:
    repo = os.environ["OMS_TEST_REPO"]
    print(f"$1\t{ppid}\tfake-socket\t.\n{repo}")
elif args == ["display-message", "-p", "-t", "%2", "#{pane_width} #{pane_height}"]:
    print("160 45")
elif args == [
    "display-message",
    "-p",
    "-t",
    "%2",
    "#{session_id}\t#{window_id}\t#{pane_pid}\t#{pane_dead}\t#{window_active}\t#{pane_in_mode}\t#{@oms_panel_main_watch}\t#{@oms_panel_main_attempt}\t#{@oms_panel_room}\t#{@oms_panel_repo}",
]:
    repo = os.environ["OMS_TEST_REPO"]
    room = os.environ["OMS_FAKE_ROOM"]
    attempt = os.environ["OMS_FAKE_ATTEMPT"]
    print(f"$1\t@1\t{ppid}\t0\t1\t0\t1\t{attempt}\t{room}\t{repo}")
elif args == ["capture-pane", "-p", "-t", "%1"]:
    print("› \n\n  GPT-6.2-Sol high · Context 9% left · weekly 13% left · main · ~/fixture · Main [default]\n← for agents · ? for shortcuts")
elif args == ["capture-pane", "-p", "-t", "%4"] or args == ["capture-pane", "-p", "-J", "-S", "-100", "-t", "%4"]:
    prompt_file = os.environ["OMS_FAKE_PROMPT_FILE"]
    with open(prompt_file, "r", encoding="utf-8") as pf:
        sys.stdout.write(pf.read())
else:
    if log:
        with open(log, "a", encoding="utf-8") as f:
            f.write(f"UNEXPECTED: {args!r}\n")
    sys.stderr.write(f"unexpected fake tmux call: {args!r}\n")
    sys.exit(1)
"##;
        std::fs::write(&fake_tmux, fake_tmux_src).unwrap();
        std::fs::set_permissions(&fake_tmux, std::fs::Permissions::from_mode(0o700)).unwrap();

        let pty_py = r#"
import fcntl, os, pty, select, struct, subprocess, sys, termios
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 160, 0, 0))
def pre():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    os.close(master)
proc = subprocess.Popen(sys.argv[1:], stdin=slave, stdout=slave, stderr=slave, preexec_fn=pre, close_fds=True)
os.close(slave)
in_fd = sys.stdin.fileno()
out_fd = sys.stdout.fileno()
def write_all(fd, data):
    while data:
        n = os.write(fd, data)
        data = data[n:]
in_open = True
while True:
    rlist = [master] + ([in_fd] if in_open else [])
    try:
        r, _, _ = select.select(rlist, [], [], 0.05)
    except InterruptedError:
        continue
    if master in r:
        try:
            data = os.read(master, 8192)
        except OSError:
            break
        if not data:
            break
        write_all(out_fd, data)
    if in_fd in r:
        data = os.read(in_fd, 8192)
        if not data:
            in_open = False
        else:
            write_all(master, data)
    if proc.poll() is not None:
        while True:
            r, _, _ = select.select([master], [], [], 0.05)
            if not r:
                break
            try:
                data = os.read(master, 8192)
            except OSError:
                break
            if not data:
                break
            write_all(out_fd, data)
        break
os.close(master)
sys.exit(proc.wait())
"#;

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

        type PtyDriver<'a> = dyn FnMut(
                &mut std::process::ChildStdin,
                &mut String,
                &mut [String],
                &mpsc::Receiver<Vec<u8>>,
            ) + 'a;
        let run_pty = |drive: &mut PtyDriver<'_>| -> ([usize; 8], String) {
            let _ = std::fs::remove_file(&log);
            let _ = std::fs::remove_file(&counters);
            let mut pty_cmd = Command::new("/usr/bin/python3");
            pty_cmd
                .arg("-c")
                .arg(pty_py)
                .arg(&exe)
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env_clear()
                .env("PATH", &bin)
                .env("HOME", &home)
                .env("TMPDIR", &tmp.0)
                .env("XDG_STATE_HOME", &state)
                .env("TERM", "xterm-256color")
                .env("TMUX_PANE", "%2")
                .env(CHILD_ENV, "1")
                .env("OMS_TEST_REPO", &repo)
                .env("OMS_TEST_EXECUTION", &wt)
                .env("OMS_TEST_COUNTERS", &counters)
                .env("OMS_FAKE_TMUX_LOG", &log)
                .env("OMS_FAKE_ROOM", &room_id)
                .env("OMS_FAKE_ATTEMPT", &attempt_id)
                .env("OMS_FAKE_LEAD_PID", lead_pid.to_string())
                .env("OMS_FAKE_PROMPT_FILE", &prompt_file)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let mut child = pty_cmd.spawn().unwrap();
            let mut child_in = child.stdin.take().unwrap();
            let mut child_out = child.stdout.take().unwrap();

            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let reader_thread = std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = child_out.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });

            let mut screen = vec![String::new(); 45];
            let mut stream = String::new();
            drive(&mut child_in, &mut stream, &mut screen, &rx);

            child_in.write_all(b"q").unwrap();
            child_in.flush().unwrap();
            let status = child.wait().unwrap();
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
        // and explicit refresh. Without a row open, no child transcript is read.
        let (pre_click_counters, pre_click_tmux_log) =
            run_pty(&mut |child_in, stream, screen, rx| {
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

                let before = count_captures();
                child_in.write_all(b"u").unwrap();
                child_in.flush().unwrap();
                wait_for(stream, screen, rx, "refresh-only second pass", &mut |s| {
                    count_captures() > before
                        && s.iter().any(|l| l.contains("2 past (Tab all)"))
                        && !s.iter().any(|l| l.contains("Refreshing Board"))
                });
            });
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
        assert_eq!(
            (pre_custody, pre_scans, pre_strict_reads, pre_legacy_reads),
            (0, 0, 0, 0),
            "passive foreground refreshes before Click must do 0 strict custody checks, 0 scans, 0 strict/legacy child reads"
        );
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

        // Run 2: Click -> Escape -> Enter -> Inode-swap close -> Re-click -> Live Turn-change close -> 0775 Unbound fallback & Silent Legacy Unavailable.
        let (run2_counters, tmux_log) = run_pty(&mut |child_in, stream, screen, rx| {
            // 1. Wait for initial fresh board, then show all tasks.
            wait_for(stream, screen, rx, "initial fresh board", &mut |s| {
                s.iter().any(|l| l.contains("2 past (Tab all)"))
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
                s.iter()
                    .any(|l| l.contains("Select an orchestrator, worker, or council"))
                    && !s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
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
            std::fs::set_permissions(&swap_tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::rename(&swap_tmp, &child_a_full).unwrap();
            child_in.write_all(b"u").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "open dock invalidation", &mut |s| {
                count_captures() > before_open_inval
                    && s.iter().any(|l| l.contains("Transcript unavailable"))
                    && s.iter()
                        .any(|l| l.contains("Select an orchestrator, worker, or council"))
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
                        && s.iter()
                            .any(|l| l.contains("Select an orchestrator, worker, or council"))
                        && !s.iter().any(|l| l.contains("OWN_CHILD_DIRECT_REPLY"))
                },
            );
            let before_restore = count_captures();
            write_prompt(&prompt_line);
            child_in.write_all(b"u").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "restore lead turn", &mut |s| {
                count_captures() > before_restore
                    && s.iter().any(|l| l.contains("Bound Researcher"))
                    && s.iter().any(|l| l.contains("Silent Legacy"))
                    && !s.iter().any(|l| l.contains("Refreshing Board"))
            });

            // 7. Chmod .git and wt to 0o775 and press 'u' to refresh into legacy mode.
            let before_captures = count_captures();
            std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o775))
                .unwrap();
            std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o775)).unwrap();
            child_in.write_all(b"u").unwrap();
            child_in.flush().unwrap();
            wait_for(stream, screen, rx, "legacy 0775 refresh", &mut |s| {
                count_captures() > before_captures
                    && s.iter().any(|l| l.contains("Bound Researcher"))
                    && s.iter().any(|l| l.contains("Silent Legacy"))
                    && !s.iter().any(|l| l.contains("Refreshing Board"))
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
                    s.iter()
                        .any(|l| l.contains("Select an orchestrator, worker, or council"))
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
            assert!(silent_text.contains("Select an orchestrator, worker, or council"));
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
        // legacy_child=0, strict_reads=3 (Click #1 + Enter #2 + Click #3),
        // leg_cand=2 (ONLY the 2 lead transcript checks when Turn switched to NEW_TURN),
        // and protected rechecks incremented custody >= 7, scans >= 3 with 0 child reads.
        assert!(!tmux_log.is_empty());
        assert!(!tmux_log.contains("UNEXPECTED:"));
        assert!(tmux_log.contains("list-panes -t %2"));
        assert!(tmux_log.contains("capture-pane -p -J -S -100 -t %4"));
        assert!(
            custody >= 7 && scans >= 3,
            "custody={custody} scans={scans}"
        );
        assert_eq!(
            (legacy_reads, strict_reads, leg_cand, leg_child),
            (0, 3, 2, 0),
            "expected exact (legacy_reads=0, strict_reads=3, leg_cand=2, leg_child=0); custody={custody} scans={scans}"
        );
        assert_eq!(legacy_reads + strict_reads, 3);

        let _ = holder.kill();
        let _ = holder.wait();
    }
}
