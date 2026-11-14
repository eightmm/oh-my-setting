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
    let room = commands::room(options).unwrap_or_default();
    let mut read = attention::load(repo, &room);
    let mut previous: Option<Value> = None;
    let mut log = eventlog::EventLog::default();
    let mut ticks = 0;
    let mut status;
    let mut last_frame: Vec<String> = vec![];
    let mut last_size = (0, 0);
    loop {
        if interrupted.load(Ordering::Acquire) {
            return Ok((130, vec![], vec![]));
        }
        let snapshot = commands::snapshot(repo, options, services, interrupted);
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
            let frame = render::frame(&report, &nav, size.0, size.1, &read);
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
                let keys = terminal.wait(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(100)),
                )?;
                let mut refresh = false;
                for key in keys {
                    let calls = render::visible(&report, &nav);
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
                            nav.notice = Some(
                                render::value_lines(&catalog["participants"], last_size.0)
                                    .join(" "),
                            );
                        }
                        navigation::Action::Controls => {
                            let mut records = Vec::new();
                            for kind in ["terminals", "jobs", "workspaces"] {
                                match oms_panel_core::terminals::PrivateStore::open(
                                    repo, kind, false, false,
                                )
                                .and_then(|s| s.list())
                                {
                                    Ok(mut rows) => records.append(&mut rows),
                                    Err(e) => nav.notice = Some(e.message),
                                }
                            }
                            nav.notice = Some(
                                render::value_lines(&Value::Array(records), last_size.0).join(" "),
                            );
                        }
                        navigation::Action::None => {}
                    }
                    let frame = render::frame(&report, &nav, last_size.0, last_size.1, &read);
                    draw(output, &frame, interactive, &mut last_frame, false)?;
                    output.flush().map_err(Error::new)?;
                }
                if refresh {
                    break;
                }
                let size = crate::layout::dimensions();
                if size != last_size {
                    last_size = size;
                    let frame = render::frame(&report, &nav, size.0, size.1, &read);
                    draw(output, &frame, interactive, &mut last_frame, true)?;
                    output.flush().map_err(Error::new)?;
                }
            } else {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Ok((status, vec![], vec![]))
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
        if force || previous.is_empty() {
            output.write_all(b"\x1b[H\x1b[2J").map_err(Error::new)?;
        }
        for row in 0..previous.len().max(frame.len()) {
            if force || frame.get(row) != previous.get(row) {
                write!(
                    output,
                    "\x1b[{};1H\x1b[2K{}",
                    row + 1,
                    frame.get(row).map(String::as_str).unwrap_or("")
                )
                .map_err(Error::new)?;
            }
        }
    } else {
        for line in frame {
            writeln!(output, "{line}").map_err(Error::new)?;
        }
    }
    *previous = frame.to_vec();
    Ok(())
}
