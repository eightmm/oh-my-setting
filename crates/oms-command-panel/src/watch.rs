use crate::{
    Error, Output, Result, Services, attention, cli::Options, commands, input, navigation, render,
};
use oms_panel_core::{NativeCommand, Value, eventlog};
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
    let room = commands::room(options).unwrap_or_default();
    let mut read = attention::load(repo, &room);
    let mut previous: Option<Value> = None;
    let mut log = eventlog::EventLog::default();
    let mut ticks = 0;
    let mut status;
    let session = commands::Session::new();
    let mut last_frame: Vec<String> = vec![];
    let mut last_size = (0, 0);
    let mut provider_rows = [None; 3];
    let mut worker_rows = std::collections::BTreeMap::new();
    let mut chat: Option<ChatDock> = None;
    let mut split = DockSplit::default();
    let mut observations = crate::orchestrator::Observations::default();
    let mut selection_changed = false;
    loop {
        if interrupted.load(Ordering::Acquire) {
            return Ok((130, vec![], vec![]));
        }
        let snapshot = commands::snapshot_with(&session, repo, options, services, interrupted);
        let mut report = match snapshot {
            Ok(state) => {
                status = state.status;
                state.report
            }
            Err(e) => {
                status = e.code;
                if let Some(state) = &previous {
                    nav.notice = Some(format!("Refresh failed: {}", e.message));
                    oms_panel_core::state::hold_state(
                        state,
                        &serde_json::json!({"error":e.message}),
                    )
                } else {
                    return Err(e);
                }
            }
        };
        if interactive
            && let Ok(panel) = crate::orchestrator::Panel::load(repo, &room, services, interrupted)
        {
            panel.observe(&mut observations, &mut report);
            if !observations.quota_pending()
                && nav
                    .notice
                    .as_deref()
                    .is_some_and(|s| s.starts_with("Checking weekly quota"))
            {
                nav.notice = None;
            }
            report["native_dock"] = serde_json::json!(panel.docked());
        }
        if let Some(old) = &previous {
            let before = render::visible(old, &nav);
            let after = render::visible(&report, &nav);
            reconcile_selection(&before, &after, &mut nav, &mut selection_changed);
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
            );
            provider_rows = frame.provider_rows;
            worker_rows = frame.worker_rows;
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
        let deadline = Instant::now() + Duration::from_secs(5);
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
                let mut changed = false;
                let mut calls_key: Option<(String, bool, crate::layout::View)> = None;
                let mut calls: Vec<Value> = Vec::new();
                for key in keys {
                    if key == input::Key::Unbound {
                        continue;
                    }
                    if report["native_dock"].is_null()
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
                    changed = true;
                    if !nav.editing
                        && !report["native_dock"].is_null()
                        && close_native_key(&key, last_size)
                        && crate::layout::dimensions() == last_size
                    {
                        match change_native_dock(
                            repo,
                            &room,
                            services,
                            interrupted,
                            None,
                            0,
                            options.get("owner"),
                        ) {
                            Ok(_) => nav.notice = None,
                            Err(e) => nav.notice = Some(e.message),
                        }
                        refresh = true;
                        changed = false;
                        break;
                    }
                    if let Some(dock) = &mut chat
                        && !nav.editing
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
                    if let input::Key::Click(x, y) = key
                        && crate::layout::dimensions() == last_size
                        && x > 0
                        && usize::from(x) <= last_size.0
                        && let Some(row) = usize::from(y).checked_sub(1)
                        && let Some(id) = worker_rows.get(&row)
                        && let Some((index, call)) = calls
                            .iter()
                            .enumerate()
                            .find(|(_, call)| &call_identity(call) == id)
                    {
                        match worker_dock(repo, call, services, interrupted) {
                            Ok(dock) => {
                                chat = Some(dock);
                                nav.selected = index;
                                nav.detail = false;
                            }
                            Err(err) => nav.notice = Some(err.message),
                        }
                        continue;
                    }
                    let identity = calls.get(nav.selected).map(call_identity);
                    let old_view = nav.view;
                    let board_height = last_size.1 - split.rows(last_size.1);
                    let page =
                        render::page_capacity(&report, &nav, last_size.0, board_height, &read);
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
                                options.get("owner"),
                            ) {
                                Ok(docked) => {
                                    report["native_dock"] = serde_json::json!(docked);
                                    chat = None;
                                    nav.notice = None;
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
                            let started = crate::orchestrator::Panel::load(
                                repo,
                                &room,
                                services,
                                interrupted,
                            )
                            .map_or(0, |panel| panel.refresh_usage(&mut observations));
                            nav.notice = Some(if started > 0 {
                                format!("Checking weekly quota for {started} provider session(s)")
                            } else {
                                "Quota refresh needs an idle Claude or AGY session".into()
                            });
                            refresh = started > 0;
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
                if changed && chat.is_some() && !report["native_dock"].is_null() {
                    match change_native_dock(
                        repo,
                        &room,
                        services,
                        interrupted,
                        None,
                        0,
                        options.get("owner"),
                    ) {
                        Ok(_) => refresh = true,
                        Err(e) => nav.notice = Some(e.message),
                    }
                    changed = false;
                }
                if changed {
                    let frame = frame(
                        &report,
                        &nav,
                        last_size,
                        &read,
                        colored,
                        interactive.then(|| split.rows(last_size.1)),
                        chat.as_mut(),
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
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
                if refresh {
                    break;
                }
                let size = crate::layout::dimensions();
                if size != last_size {
                    if follow {
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
                    );
                    provider_rows = frame.provider_rows;
                    worker_rows = frame.worker_rows;
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
}

fn worker_dock(
    repo: &Path,
    call: &Value,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<ChatDock> {
    let title = crate::text(call, "title").trim();
    let title = if title.is_empty() {
        crate::text(call, "participant")
    } else {
        title
    };
    let title = format!("{title} · {}", crate::text(call, "role"));
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
    if history.trim().is_empty() {
        history = "No result recorded for this worker yet".into();
    }
    Ok(ChatDock {
        title,
        history,
        back: 0,
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

fn frame(
    report: &Value,
    nav: &navigation::Navigation,
    size: (usize, usize),
    read: &std::collections::BTreeMap<String, String>,
    colored: bool,
    dock_rows: Option<usize>,
    chat: Option<&mut ChatDock>,
) -> render::BoardFrame {
    let (columns, height) = size;
    let interactive = dock_rows.is_some();
    if interactive && let Some(provider) = report["native_dock"].as_str() {
        let board_height = height.saturating_sub(1);
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
    let dock = dock_rows.unwrap_or(0);
    if dock < 3 {
        return render::board_frame(report, nav, columns, height, read, colored, interactive);
    }
    let mut board = render::board_frame(
        report,
        nav,
        columns,
        height - dock,
        read,
        colored,
        interactive,
    );
    let top = height - dock;
    board.lines.resize(top, String::new());
    for hit in &mut board.provider_rows {
        if hit.is_some_and(|row| row >= top) {
            *hit = None;
        }
    }
    board.worker_rows.retain(|row, _| *row < top);
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
    board.lines.push(render::dock_top(title, columns, colored));
    let mut visible = visible;
    visible.resize(page, String::new());
    board.lines.extend(visible.into_iter().map(|line| {
        if boxed {
            render::dock_line(&line, columns)
        } else {
            line
        }
    }));
    if boxed {
        board.lines.push(render::dock_bottom(columns, colored));
    }
    board.lines.push(render::clip(hint, columns));
    board
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

fn reconcile_selection(
    before: &[Value],
    after: &[Value],
    nav: &mut navigation::Navigation,
    changed: &mut bool,
) {
    if let Some(id) = before.get(nav.selected).map(call_identity)
        && !after.iter().any(|call| call_identity(call) == id)
    {
        *changed = true;
        nav.detail = false;
    }
    nav.selected = preserve_selection(before, after, nav.selected);
}

fn apply_selection_key(
    nav: &mut navigation::Navigation,
    changed: &mut bool,
    key: &input::Key,
    count: usize,
    page: usize,
) -> navigation::Action {
    if *changed && matches!(key, input::Key::Enter | input::Key::Char('f')) && !nav.editing {
        *changed = false;
        nav.notice = Some("selection changed; press Enter again to open".into());
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
        *changed = false;
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
        };
        let report = serde_json::json!({
            "collection": {"ok": true},
            "plan": {"goal": "Current work"},
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
    fn divider_keeps_its_row_when_switching_content_at_short_heights() {
        let report = serde_json::json!({"collection": {"ok": true}});
        let nav = Default::default();
        let read = Default::default();
        let mut chat = super::ChatDock {
            title: "AGY · Recent chat".into(),
            history: "\x1b[31m한글 reply\x1b[0m ".repeat(40),
            back: 0,
        };
        let split = super::DockSplit::default();
        for size @ (columns, height) in [(38, 12), (80, 17), (80, 23), (140, 90)] {
            let rows = split.rows(height);
            let divider = height - rows;
            let idle = super::frame(&report, &nav, size, &read, true, Some(rows), None);
            let opened = super::frame(
                &report,
                &nav,
                size,
                &read,
                true,
                Some(rows),
                Some(&mut chat),
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
        let mut changed = false;
        super::reconcile_selection(&[a, b.clone()], &[b], &mut nav, &mut changed);
        assert!(!nav.detail);
        assert!(changed);
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
        let mut changed = false;
        super::reconcile_selection(&old, &new, &mut nav, &mut changed);
        assert!(changed);
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
}
