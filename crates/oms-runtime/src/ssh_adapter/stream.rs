//! Stream artifacts through the same owned reactor used by native commands.
use super::*;
use oms_supervisor::capture::{CaptureOptions, LimitAction};
use oms_supervisor::process::{
    DuplexOptions, OutputEvent, OutputStream, Progress, Reactor, Request, StopReason, Streams,
};

fn consume(
    event: &OutputEvent,
    file: &mut impl Write,
    total: &mut u64,
    hasher: &mut Sha256,
    limit: u64,
) -> Result<()> {
    if event.stream == OutputStream::Stdout {
        *total = total.saturating_add(event.bytes.len() as u64);
        require(
            *total <= limit,
            format!("collected stream exceeded --max-bytes ({limit})"),
        )?;
        file.write_all(&event.bytes).map_err(error)?;
        hasher.update(&event.bytes);
    }
    Ok(())
}

pub(super) fn collect(
    req: &CollectRequest<'_>,
    script: &str,
    file: &mut impl Write,
) -> Result<(u64, String)> {
    resumable::check_request(req)?;
    let mut command = build_ssh(req.host, req.port, req.control);
    command.arg(format!("sh -lc {}", quote(script)));
    let argv = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(OsStr::to_os_string)
        .collect();
    let mut request = Request::new(argv);
    request.cwd = Some(req.cwd.into());
    request.streams = Streams::Separate;
    request.deadline = Some(req.deadline);
    request.terminate_grace = Duration::from_millis(250);
    // The disk sink receives every byte via bounded events. Keeping a second
    // in-memory artifact capture would defeat streaming at the 512 MiB limit.
    request.capture = CaptureOptions {
        max_bytes: 0,
        on_limit: LimitAction::Drain,
        lines: None,
    };
    request.stderr_capture = Some(CaptureOptions {
        max_bytes: MAX_STDERR_BYTES as usize,
        on_limit: LimitAction::Drain,
        lines: None,
    });
    request.duplex = Some(DuplexOptions {
        max_frame_bytes: 1,
        max_input_bytes: 1,
        max_input_frames: 1,
        max_output_bytes: 64 * 1024,
        max_output_events: 8,
        max_read_bytes_per_turn: 64 * 1024,
        max_write_bytes_per_turn: 1,
    });
    let mut reactor = Reactor::new();
    let job = reactor.start(request).map_err(error)?;
    reactor.close_input(job).map_err(error)?;
    let mut total = 0;
    let mut hasher = Sha256::new();
    let mut failure = None;
    let mut stopping = None;
    loop {
        while let Some(event) = reactor.receive(job).map_err(error)? {
            if failure.is_none()
                && let Err(e) = consume(&event, file, &mut total, &mut hasher, req.max_bytes)
            {
                failure = Some(e);
                reactor
                    .stop(job, StopReason::ConsumerCutoff)
                    .map_err(error)?;
                stopping.get_or_insert_with(Instant::now);
            }
        }
        if req.interrupted.load(Ordering::Relaxed) {
            reactor.stop(job, StopReason::Cancelled).map_err(error)?;
            stopping.get_or_insert_with(Instant::now);
        }
        match reactor
            .poll(job, Duration::from_millis(10))
            .map_err(error)?
        {
            Progress::Pending(snapshot) => {
                if snapshot.stop.is_some() {
                    stopping.get_or_insert_with(Instant::now);
                }
                if stopping.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
                    // Drop makes a bounded final cleanup attempt; a stuck kernel
                    // child is never reported as confirmed cleanup or delivery.
                    return Err(status(
                        75,
                        "collection stopped; local SSH cleanup is unknown",
                    ));
                }
            }
            Progress::Complete(done) => {
                if let Some(duplex) = &done.duplex {
                    for event in &duplex.output {
                        if failure.is_none()
                            && let Err(e) =
                                consume(event, file, &mut total, &mut hasher, req.max_bytes)
                        {
                            failure = Some(e);
                        }
                    }
                }
                if !done.cleanup_complete() {
                    return Err(status(
                        75,
                        "collection stopped; local SSH cleanup is unknown",
                    ));
                }
                if let Some(e) = failure {
                    return Err(e);
                }
                match done.stop {
                    Some(StopReason::Cancelled | StopReason::Signal(_)) => {
                        return Err(status(130, "collection interrupted; destination unchanged"));
                    }
                    Some(StopReason::Deadline) => {
                        return Err(status(
                            124,
                            "collection request timed out; destination unchanged",
                        ));
                    }
                    Some(_) => return Err(status(75, "collection stopped before complete output")),
                    None => {}
                }
                require(done.output_complete(), "collection output is incomplete")?;
                let exit = done.exit.map_or(1, |exit| exit.shell_code());
                if exit != 0 {
                    let stderr = done
                        .stderr
                        .as_ref()
                        .map_or(&[][..], |capture| capture.bytes.as_slice());
                    return Err(status(
                        exit,
                        format!(
                            "remote collect failed (exit {exit}): {}",
                            String::from_utf8_lossy(stderr).trim()
                        ),
                    ));
                }
                return Ok((total, hasher.finish()));
            }
        }
    }
}
