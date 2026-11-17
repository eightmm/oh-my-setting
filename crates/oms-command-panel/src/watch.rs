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
    loop {
        if interrupted.load(Ordering::Acquire) {
            return Ok((130, vec![], vec![]));
        }
        let snapshot = commands::snapshot_with(&session, repo, options, services, interrupted);
        let report = match snapshot {
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
        if logging {
            for line in log.lines(&report, &oms_panel_core::now()) {
                writeln!(output, "{}", render::clean(&line, 4096)).map_err(Error::new)?;
            }
        } else {
            let size = crate::layout::dimensions();
            let frame =
                render::frame_styled(&report, &nav, size.0, size.1, &read, colored, interactive);
            draw(
                output,
                &frame,
                interactive,
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
                let mut calls_key: Option<(String, bool)> = None;
                let mut calls: Vec<Value> = Vec::new();
                for key in keys {
                    if key == input::Key::Unbound {
                        continue;
                    }
                    changed = true;
                    let want = (nav.filter.clone(), nav.attention_only);
                    if calls_key.as_ref() != Some(&want) {
                        calls = render::visible(&report, &nav);
                        calls_key = Some(want);
                    }
                    let action = nav.apply(&key, calls.len(), last_size.1);
                    match action {
                        navigation::Action::Quit => return Ok((0, vec![], vec![])),
                        navigation::Action::Refresh => refresh = true,
                        navigation::Action::ReadAll => {
                            let items = attention::items(&report, &read);
                            match attention::mark(repo, &room, &report, &items) {
                                Ok(()) => read = attention::load(repo, &room),
                                Err(e) => nav.notice = Some(e.message),
                            }
                        }
                        navigation::Action::Open => {
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
                if changed {
                    let frame = render::frame_styled(
                        &report,
                        &nav,
                        last_size.0,
                        last_size.1,
                        &read,
                        colored,
                        interactive,
                    );
                    draw(output, &frame, interactive, &mut last_frame, false)?;
                    output.flush().map_err(Error::new)?;
                }
                if refresh {
                    break;
                }
                let size = crate::layout::dimensions();
                if size != last_size {
                    last_size = size;
                    let frame = render::frame_styled(
                        &report,
                        &nav,
                        size.0,
                        size.1,
                        &read,
                        colored,
                        interactive,
                    );
                    draw(output, &frame, interactive, &mut last_frame, true)?;
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
        if full || previous.len() > frame.len() {
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
    use super::draw;
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
    fn tty_draw_is_one_synchronized_overwrite_without_blanking() {
        let mut out = Counting(vec![], 0);
        let mut previous = vec![];
        draw(
            &mut out,
            &rows(&["a", "b", "c"]),
            true,
            &mut previous,
            false,
        )
        .unwrap();
        assert_eq!(out.1, 1, "one write per frame");
        let first = String::from_utf8(std::mem::take(&mut out.0)).unwrap();
        assert!(first.starts_with("\x1b[?2026h") && first.ends_with("\x1b[?2026l"));
        assert!(!first.contains("\x1b[2J") && !first.contains("\x1b[H"));
        // Only the changed row is rewritten; shrinking clears the tail.
        draw(&mut out, &rows(&["a", "x"]), true, &mut previous, false).unwrap();
        let second = String::from_utf8(std::mem::take(&mut out.0)).unwrap();
        assert!(second.contains("\x1b[2;1H\x1b[2Kx"));
        assert!(!second.contains("\x1b[1;1H"));
        assert!(second.contains("\x1b[3;1H\x1b[J"));
        // An unchanged frame writes nothing; resize forces a full overwrite.
        let writes = out.1;
        draw(&mut out, &rows(&["a", "x"]), true, &mut previous, false).unwrap();
        assert_eq!(out.1, writes);
        draw(&mut out, &rows(&["a", "x"]), true, &mut previous, true).unwrap();
        assert!(String::from_utf8_lossy(&out.0).contains("\x1b[1;1H"));
        assert!(!String::from_utf8_lossy(&out.0).contains("\x1b[2J"));
    }
}
