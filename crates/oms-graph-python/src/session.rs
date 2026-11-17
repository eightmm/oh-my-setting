//! Client side of the helper boundary: one persistent private helper process
//! per [`Session`].
//!
//! Lifecycle: [`Session::start`] spawns the helper from an explicit absolute
//! path (no PATH lookup, no shell, empty environment, stderr discarded) and two
//! I/O threads that own its pipes. Parses reuse that process, one at a time
//! (`&mut self` is the serialization; use one session per concurrent consumer).
//! Any infrastructure failure (exit or signal, timeout, cancellation, framing
//! violation) kills and reaps the child, closes the pipes and joins both
//! threads before the error returns, and leaves the session `Poisoned`: it
//! never restarts or retries by itself, the caller starts a new session.
//! Dropping a session does the same cleanup.
//!
//! Buffering is bounded: at most one request and one response frame exist, the
//! response ceiling is checked before its payload is read, and the reader hands
//! over frames through a one-slot channel.

use std::io::{self, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::protocol::{
    FrameError, KIND_REFUSAL, KIND_REQUEST, KIND_RESULT, MAX_FIELD_BYTES, MAX_RESPONSE_BYTES,
    MAX_SOURCE_BYTES, REFUSE_RESPONSE, REFUSE_SOURCE, Request, encode_request, read_frame,
    write_frame,
};
use crate::{Error, ParseResult};

/// Poll slice while waiting for a response, so cancellation is prompt.
const POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Larger sources are refused with [`Error::SourceTooLarge`]; never more
    /// than [`MAX_SOURCE_BYTES`].
    pub max_bytes: usize,
    /// Whole-request deadline; the helper is killed when it passes.
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_bytes: crate::DEFAULT_MAX_BYTES,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Cooperative cancellation of one parse, checked while it waits.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What a consumer needs from a parser; [`Session`] is the production one and
/// tests may inject another.
pub trait Parser {
    fn parse(&mut self, path: &str, text: &str, source_digest: &str) -> Result<ParseResult, Error>;
}

enum Incoming {
    Frame(crate::protocol::Header, Vec<u8>),
    /// The stream ended or broke; the child's exit status is the explanation.
    Ended(Option<&'static str>),
}

pub struct Session {
    child: Option<Child>,
    pid: u32,
    requests: Option<SyncSender<Vec<u8>>>,
    responses: Option<Receiver<Incoming>>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
    options: Options,
    poisoned: bool,
}

fn describe(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("helper killed by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("helper exited with status {code}"),
        None => "helper ended abnormally".to_owned(),
    }
}

/// Address space the helper may use: a fixed base plus room for the AST of the
/// largest source this session accepts.
fn memory_mib(options: &Options) -> u64 {
    let source_mib = options
        .max_bytes
        .min(MAX_SOURCE_BYTES)
        .div_ceil(1024 * 1024) as u64;
    (512 + 64 * source_mib).min(8192)
}

impl Session {
    /// Start a helper from `helper`, which must be the packaged private helper
    /// executable at an absolute path.
    pub fn start(helper: &Path, options: Options) -> Result<Self, Error> {
        Self::start_with(helper, options, &mut spawn_thread)
    }

    /// [`Session::start`] with the thread spawner injected, so a unit test can
    /// fail exactly one of the two I/O threads.
    fn start_with(helper: &Path, options: Options, spawn: &mut Spawner<'_>) -> Result<Self, Error> {
        if !helper.is_absolute() || !helper.is_file() {
            return Err(Error::HelperUnavailable(
                "helper path must be an absolute path to a file".to_owned(),
            ));
        }
        let mut child = Command::new(helper)
            .arg("--memory-mib")
            .arg(memory_mib(&options).to_string())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Error::HelperUnavailable(error.kind().to_string()))?;
        let pid = child.id();
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::HelperUnavailable("helper pipes missing".to_owned()));
        };
        let (requests, request_rx) = sync_channel::<Vec<u8>>(1);
        let (response_tx, responses) = sync_channel::<Incoming>(1);
        let writer = spawn(
            "oms-graph-python-writer",
            Box::new(move || write_loop(stdin, request_rx)),
        );
        let reader = match writer {
            Ok(_) => spawn(
                "oms-graph-python-reader",
                Box::new(move || read_loop(stdout, response_tx)),
            ),
            Err(_) => Err(io::Error::other("writer thread not started")),
        };
        let (writer, reader) = match (writer, reader) {
            (Ok(writer), Ok(reader)) => (writer, reader),
            (writer, reader) => {
                // Kill and reap the child, close our channel ends so a thread
                // that did start unblocks, and join it before returning.
                let _ = child.kill();
                let _ = child.wait();
                drop(requests);
                drop(responses);
                for handle in [writer.ok(), reader.ok()].into_iter().flatten() {
                    let _ = handle.join();
                }
                return Err(Error::HelperUnavailable(
                    "cannot start I/O threads".to_owned(),
                ));
            }
        };
        Ok(Self {
            child: Some(child),
            pid,
            requests: Some(requests),
            responses: Some(responses),
            writer: Some(writer),
            reader: Some(reader),
            next_id: 1,
            options,
            poisoned: false,
        })
    }

    /// Process id of the helper, stable for the life of the session.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn parse(
        &mut self,
        path: &str,
        text: &str,
        source_digest: &str,
    ) -> Result<ParseResult, Error> {
        self.parse_cancellable(path, text, source_digest, &CancelFlag::new())
    }

    /// [`Session::parse`] that stops, kills the helper and returns
    /// [`Error::Cancelled`] once `cancel` is set, checked before any work, while
    /// waiting and when a reply arrives. `Options::timeout` is enforced to the
    /// millisecond (zero never succeeds); a timeout too large for the clock
    /// means no deadline, so only cancellation or a helper failure ends the wait.
    pub fn parse_cancellable(
        &mut self,
        path: &str,
        text: &str,
        source_digest: &str,
        cancel: &CancelFlag,
    ) -> Result<ParseResult, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        // The deadline starts here, before the encoding and sending it covers.
        // A timeout too large to add to the clock is an unbounded wait that
        // only cancellation or a helper failure ends.
        let deadline = Instant::now().checked_add(self.options.timeout);
        // Cancellation outranks everything after it, size errors included, so
        // a caller looping over many small files stops at the first check.
        if cancel.is_cancelled() {
            return Err(self.fail(Error::Cancelled));
        }
        let limit = self.options.max_bytes.min(MAX_SOURCE_BYTES);
        if text.len() > limit {
            return Err(Error::SourceTooLarge {
                bytes: text.len(),
                max_bytes: limit,
            });
        }
        for (field, value) in [("path", path), ("source_digest", source_digest)] {
            if value.len() > MAX_FIELD_BYTES {
                return Err(Error::FieldTooLong {
                    field,
                    max: MAX_FIELD_BYTES,
                });
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let payload = encode_request(&Request {
            max_bytes: limit as u32,
            path,
            digest: source_digest,
            source: text,
        });
        let mut frame = Vec::with_capacity(payload.len() + crate::protocol::HEADER_LEN);
        if write_frame(&mut frame, KIND_REQUEST, id, &payload).is_err() {
            return Err(self.fail(Error::Protocol("request cannot be framed".to_owned())));
        }
        drop(payload);
        if let Some(error) = self.interrupted(cancel, deadline) {
            return Err(self.fail(error));
        }
        let sent = self
            .requests
            .as_ref()
            .is_some_and(|requests| requests.send(frame).is_ok());
        if !sent {
            return Err(self.fail_exited());
        }
        loop {
            // Cancellation first, then the deadline; a reply that is already
            // waiting does not outrank either.
            if let Some(error) = self.interrupted(cancel, deadline) {
                return Err(self.fail(error));
            }
            let slice = deadline.map_or(POLL, |end| {
                end.saturating_duration_since(Instant::now()).min(POLL)
            });
            let Some(responses) = self.responses.as_ref() else {
                return Err(Error::Poisoned);
            };
            match responses.recv_timeout(slice) {
                Ok(Incoming::Frame(header, payload)) => {
                    if let Some(error) = self.interrupted(cancel, deadline) {
                        return Err(self.fail(error));
                    }
                    return self.finish(
                        id,
                        header,
                        &payload,
                        limit,
                        text.len(),
                        (cancel, deadline),
                    );
                }
                Ok(Incoming::Ended(reason)) => {
                    return Err(match reason {
                        Some(reason) => self.fail(Error::Protocol(reason.to_owned())),
                        None => self.fail_exited(),
                    });
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.fail_exited()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Cancellation, or an expired deadline, as the error to fail with.
    fn interrupted(&self, cancel: &CancelFlag, deadline: Option<Instant>) -> Option<Error> {
        if cancel.is_cancelled() {
            return Some(Error::Cancelled);
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            let millis = u64::try_from(self.options.timeout.as_millis()).unwrap_or(u64::MAX);
            return Some(Error::Timeout { millis });
        }
        None
    }

    fn finish(
        &mut self,
        id: u64,
        header: crate::protocol::Header,
        payload: &[u8],
        limit: usize,
        source_bytes: usize,
        stop: (&CancelFlag, Option<Instant>),
    ) -> Result<ParseResult, Error> {
        let (cancel, deadline) = stop;
        if header.id != id {
            return Err(self.fail(Error::Protocol("response identity mismatch".to_owned())));
        }
        match header.kind {
            KIND_RESULT => match decode_result(payload, || self.interrupted(cancel, deadline)) {
                Ok(result) => match self.interrupted(cancel, deadline) {
                    Some(error) => Err(self.fail(error)),
                    None => Ok(result),
                },
                Err(Decode::Interrupted(error)) => Err(self.fail(error)),
                Err(Decode::Malformed) => {
                    Err(self.fail(Error::Protocol("malformed result".to_owned())))
                }
            },
            KIND_REFUSAL if payload == REFUSE_SOURCE => Err(Error::SourceTooLarge {
                bytes: source_bytes,
                max_bytes: limit,
            }),
            KIND_REFUSAL if payload == REFUSE_RESPONSE => Err(Error::ResponseTooLarge {
                max_bytes: MAX_RESPONSE_BYTES,
            }),
            _ => Err(self.fail(Error::Protocol("unexpected response kind".to_owned()))),
        }
    }

    /// Tear down after `error` and return it.
    fn fail(&mut self, error: Error) -> Error {
        self.teardown();
        error
    }

    fn fail_exited(&mut self) -> Error {
        let status = self.teardown();
        Error::HelperExited(status.map_or_else(|| "helper ended".to_owned(), describe))
    }

    /// Kill and reap the child, close the pipes, join both threads.
    fn teardown(&mut self) -> Option<ExitStatus> {
        self.poisoned = true;
        let mut status = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            status = child.wait().ok();
        }
        self.requests = None;
        self.responses = None;
        for handle in [self.writer.take(), self.reader.take()]
            .into_iter()
            .flatten()
        {
            let _ = handle.join();
        }
        status
    }

    /// Stop the helper now; same as dropping the session.
    pub fn close(mut self) {
        self.teardown();
    }
}

impl Parser for Session {
    fn parse(&mut self, path: &str, text: &str, source_digest: &str) -> Result<ParseResult, Error> {
        Session::parse(self, path, text, source_digest)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.teardown();
    }
}

type Spawner<'a> =
    dyn FnMut(&'static str, Box<dyn FnOnce() + Send + 'static>) -> io::Result<JoinHandle<()>> + 'a;

fn spawn_thread(
    name: &'static str,
    body: Box<dyn FnOnce() + Send + 'static>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name(name.to_owned()).spawn(body)
}

enum Decode {
    Interrupted(Error),
    Malformed,
}

/// Bytes the decoder consumes between interruption checks.
const DECODE_CHUNK: usize = 64 * 1024;

/// Reads a payload slice in chunks and asks `interrupted` before each one, so
/// a large result stops decoding when cancellation or the deadline arrives.
struct Checked<'a, F: FnMut() -> Option<Error>> {
    data: &'a [u8],
    interrupted: F,
    stopped: Option<Error>,
}

impl<F: FnMut() -> Option<Error>> Read for Checked<'_, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.stopped.is_none() {
            self.stopped = (self.interrupted)();
        }
        if self.stopped.is_some() {
            return Err(io::Error::other("interrupted"));
        }
        let take = self.data.len().min(buf.len()).min(DECODE_CHUNK);
        buf[..take].copy_from_slice(&self.data[..take]);
        self.data = &self.data[take..];
        Ok(take)
    }
}

/// `serde_json::from_slice::<ParseResult>` that is interruptible: the typed
/// result and its limits are the same, only the reads are chunked and checked.
fn decode_result(
    payload: &[u8],
    interrupted: impl FnMut() -> Option<Error>,
) -> Result<ParseResult, Decode> {
    let mut reader = BufReader::with_capacity(
        DECODE_CHUNK,
        Checked {
            data: payload,
            interrupted,
            stopped: None,
        },
    );
    let outcome = serde_json::from_reader::<_, ParseResult>(&mut reader);
    match (outcome, reader.into_inner().stopped) {
        (_, Some(error)) => Err(Decode::Interrupted(error)),
        (Ok(result), None) => Ok(result),
        (Err(_), None) => Err(Decode::Malformed),
    }
}

fn write_loop(mut stdin: ChildStdin, requests: Receiver<Vec<u8>>) {
    for frame in requests {
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

fn read_loop(mut stdout: ChildStdout, responses: SyncSender<Incoming>) {
    loop {
        let message = match read_frame(&mut stdout, MAX_RESPONSE_BYTES) {
            Ok(Some((header, payload))) => Incoming::Frame(header, payload),
            Ok(None) | Err(FrameError::Truncated | FrameError::Io) => Incoming::Ended(None),
            Err(FrameError::Invalid(reason)) => Incoming::Ended(Some(reason)),
        };
        let last = matches!(message, Incoming::Ended(_));
        if responses.send(message).is_err() || last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn big_result() -> Vec<u8> {
        let mut result = ParseResult::default();
        for index in 0..4_000 {
            result.refs.push(crate::Reference {
                from: format!("file:m{index}.py"),
                relation: "calls".to_owned(),
                kind: "name".to_owned(),
                value: "target".to_owned(),
                line: index,
                binding: None,
                imported: None,
                scoped: None,
                method_call: None,
            });
        }
        result.to_json().into_bytes()
    }

    #[test]
    fn decoding_matches_from_slice_and_stops_between_chunks() {
        let payload = big_result();
        assert!(payload.len() > 4 * DECODE_CHUNK);
        let whole: ParseResult = serde_json::from_slice(&payload).unwrap();
        assert!(matches!(decode_result(&payload, || None), Ok(r) if r == whole));
        assert!(matches!(
            decode_result(b"{}", || None),
            Err(Decode::Malformed)
        ));
        assert!(matches!(
            decode_result(&payload[..payload.len() - 1], || None),
            Err(Decode::Malformed)
        ));
        // Interrupted on the third check: it stops there, not after the whole payload.
        let mut checks = 0;
        let outcome = decode_result(&payload, || {
            checks += 1;
            (checks == 3).then_some(Error::Cancelled)
        });
        assert!(matches!(
            outcome,
            Err(Decode::Interrupted(Error::Cancelled))
        ));
        assert_eq!(checks, 3);
        // Interrupted before the first byte.
        let outcome = decode_result(&payload, || Some(Error::Timeout { millis: 0 }));
        assert!(matches!(
            outcome,
            Err(Decode::Interrupted(Error::Timeout { millis: 0 }))
        ));
    }

    /// Any native executable will do: startup fails before it is used, and it
    /// is killed and reaped either way (the test binary itself, here).
    fn helper() -> std::path::PathBuf {
        std::env::current_exe().expect("test binary")
    }

    #[test]
    fn a_partly_started_io_thread_pair_is_joined_and_the_child_reaped() {
        for fail_at in ["oms-graph-python-writer", "oms-graph-python-reader"] {
            let finished = Arc::new(AtomicBool::new(false));
            let started = Arc::new(AtomicBool::new(false));
            let (flag, begun) = (finished.clone(), started.clone());
            let mut spawn = move |name: &'static str, body: Box<dyn FnOnce() + Send>| {
                if name == fail_at {
                    return Err(io::Error::other("injected spawn failure"));
                }
                begun.store(true, Ordering::SeqCst);
                let flag = flag.clone();
                thread::Builder::new().name(name.to_owned()).spawn(move || {
                    body();
                    flag.store(true, Ordering::SeqCst);
                })
            };
            let error = Session::start_with(&helper(), Options::default(), &mut spawn)
                .err()
                .expect("startup fails");
            assert_eq!(error.code(), "helper_unavailable", "{fail_at}");
            // A thread that did start has been joined, not left behind.
            assert_eq!(
                started.load(Ordering::SeqCst),
                finished.load(Ordering::SeqCst),
                "{fail_at}"
            );
            assert_eq!(
                started.load(Ordering::SeqCst),
                fail_at == "oms-graph-python-reader"
            );
        }
    }
}
