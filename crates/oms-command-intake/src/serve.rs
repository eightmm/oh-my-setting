//! Source-neutral loopback HTTP intake listener and Room `question` mapper.

use crate::crypto::{
    SecretStore, TIMESTAMP_WINDOW_MS, current_unix_ms, verify_hmac_sha256_hex,
    within_timestamp_window_ms,
};
use crate::model::{
    WorkRequest, format_work_request_room_text, intake_path, sanitize_log_id, validate_source_name,
};
use crate::routes::{Route, resolve};
use crate::source::{
    BindingDecision, OutboundAck, PayloadDecision, RequestHeaders, RoomDelivery, Source,
    TransportError,
};
use oms_command_room::{Room, Send as RoomSend, Services as RoomServices, Session as RoomSession};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read as _, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8787";
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
pub const ACK_WINDOW: Duration = Duration::from_secs(10);
pub const ACCEPT_BACKOFF_INITIAL: Duration = Duration::from_millis(100);
pub const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);
const BG_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const BG_QUEUE_CAP: usize = 64;
const POLL_SLICE: Duration = Duration::from_millis(50);

pub const ROOM_CONTENT_REFUSAL_MESSAGES: &[&str] = &[
    "turn looks sensitive; it would be replayed into other agents' prompts",
    "room metadata contains sensitive-looking content or could not be checked",
    "message needs 1..4000 UTF-8 bytes",
    "--text or --text-file is required",
    "room metadata cannot be encoded as UTF-8",
    "room metadata is oversized",
];
pub const ROOM_DUPLICATE_CONFLICT_MESSAGE: &str = "message id conflicts with its existing content";

#[derive(Clone)]
pub struct ServeConfig {
    pub source: Arc<dyn Source>,
    /// Room targets by team; the single-target `--repo` binding is one `*` route.
    pub routes: Vec<Route>,
    pub binding_flags: BTreeMap<String, String>,
    pub listen: SocketAddr,
    pub secrets: SecretStore,
}

pub fn parse_loopback_listen(raw: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = raw
        .parse()
        .map_err(|_| format!("invalid --listen address '{raw}': expected LOOPBACK_IP:PORT"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing non-loopback --listen address '{addr}': oms intake serve binds loopback only"
        ));
    }
    Ok(addr)
}

pub fn validate_serve_room(
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> Result<(), String> {
    validate_source_name(cfg.source.name())?;
    if cfg.routes.is_empty() {
        return Err(format!(
            "no intake routes for source '{}' (add one with `oms intake route add`)",
            cfg.source.name()
        ));
    }
    cfg.source.validate_binding(&cfg.binding_flags)?;
    for spec in cfg.source.serve_env() {
        if spec.sensitive && spec.required && cfg.secrets.get(spec.name).is_none() {
            return Err(format!(
                "missing required environment variable {}",
                spec.name
            ));
        }
    }
    cfg.source.validate_endpoint()?;
    for (i, route) in cfg.routes.iter().enumerate() {
        crate::routes::validate_team(&route.team)?;
        if route.source != cfg.source.name() {
            return Err(format!(
                "route team {} belongs to source '{}', not '{}'",
                route.team,
                route.source,
                cfg.source.name()
            ));
        }
        if cfg.routes[..i].iter().any(|r| r.team == route.team) {
            return Err(format!("two intake routes for team {}", route.team));
        }
    }
    for route in &cfg.routes {
        validate_route_room(route, room_services, interrupted).map_err(|e| {
            if cfg.routes.len() == 1 && route.team == crate::routes::ANY_TEAM {
                e
            } else {
                format!("route team {}: {e}", route.team)
            }
        })?;
    }
    Ok(())
}

/// The route's Room exists, is open, and enrolls both the sender and the recipient.
pub fn validate_route_room(
    route: &Route,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> Result<(), String> {
    oms_command_room::identifier(&route.room, "room").map_err(|e| e.message)?;
    oms_command_room::identifier(&route.participant, "participant").map_err(|e| e.message)?;
    if route.to != "all" {
        oms_command_room::identifier(&route.to, "recipient").map_err(|e| e.message)?;
    }
    if route.participant == route.to {
        return Err("--participant and --to must name distinct Room participants".into());
    }
    let room = Room::open(
        &route.repo,
        RoomSession::from_env(),
        room_services,
        interrupted,
        false,
        false,
    )
    .map_err(|e| format!("room unavailable: {}", e.message))?;
    let state = room
        .status(&route.room, false)
        .map_err(|e| format!("room '{}' unavailable: {}", route.room, e.message))?;
    if state.flag("closed") {
        return Err(format!("room '{}' is closed", route.room));
    }
    oms_command_room::participant(&state, &route.participant)
        .map_err(|e| format!("--participant '{}': {}", route.participant, e.message))?;
    if route.to != "all" {
        oms_command_room::participant(&state, &route.to)
            .map_err(|e| format!("--to '{}': {}", route.to, e.message))?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct AcceptBackoff {
    next_delay: Duration,
    active_error: Option<(std::io::ErrorKind, Option<i32>)>,
}

impl Default for AcceptBackoff {
    fn default() -> Self {
        Self::new()
    }
}

impl AcceptBackoff {
    pub const fn new() -> Self {
        Self {
            next_delay: ACCEPT_BACKOFF_INITIAL,
            active_error: None,
        }
    }

    /// Record an `accept` failure and return `(backoff_delay, is_new_distinct_burst)`.
    pub fn on_error(&mut self, err: &std::io::Error) -> (Duration, bool) {
        let sig = (err.kind(), err.raw_os_error());
        let is_new_burst = self.active_error != Some(sig);
        self.active_error = Some(sig);
        let delay = self.next_delay;
        self.next_delay = self.next_delay.saturating_mul(2).min(ACCEPT_BACKOFF_MAX);
        (delay, is_new_burst)
    }

    pub fn on_success(&mut self) {
        self.next_delay = ACCEPT_BACKOFF_INITIAL;
        self.active_error = None;
    }
}

struct QueuedAck {
    ack: OutboundAck,
    deadline: Instant,
}

fn ack_error_kind(err: &TransportError) -> &'static str {
    match err {
        TransportError::OutcomeUnknown(_) => "outcome_unknown",
        TransportError::Rejected(_) => "rejected",
    }
}

fn deliver_queued_ack(
    source: &dyn Source,
    secrets: &SecretStore,
    queued: &QueuedAck,
    interrupted: &AtomicBool,
    stop: &AtomicBool,
    log_tx: &mpsc::Sender<String>,
) {
    let ack = &queued.ack;
    let first_timeout = queued
        .deadline
        .saturating_duration_since(Instant::now())
        .min(BG_ACK_TIMEOUT);
    let Err(first_err) = source.deliver_ack(secrets, ack, first_timeout, interrupted) else {
        return;
    };
    if interrupted.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return;
    }
    let remaining = queued.deadline.saturating_duration_since(Instant::now());
    let can_retry = !remaining.is_zero();
    let source_name = source.name();
    let session = sanitize_log_id(Some(&ack.session_ref));
    let kind = sanitize_log_id(Some(&ack.kind));
    let err_kind = ack_error_kind(&first_err);
    let retry_note = if can_retry { "once" } else { "none" };
    let _ = log_tx.send(format!(
        "intake serve: outbound ack failed: source={source_name} session={session} kind={kind} error={err_kind} retry={retry_note}"
    ));
    if !can_retry || interrupted.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
        return;
    }
    let retry_timeout = remaining.min(BG_ACK_TIMEOUT);
    if let Err(retry_err) = source.deliver_ack(secrets, ack, retry_timeout, interrupted)
        && !interrupted.load(Ordering::Acquire)
        && !stop.load(Ordering::Acquire)
    {
        let retry_err_kind = ack_error_kind(&retry_err);
        let _ = log_tx.send(format!(
            "intake serve: outbound ack retry failed: source={source_name} session={session} kind={kind} error={retry_err_kind}"
        ));
    }
}

fn flush_worker_logs(log_rx: &mpsc::Receiver<String>, stderr: &mut dyn Write) {
    let mut wrote = false;
    while let Ok(line) = log_rx.try_recv() {
        let _ = writeln!(stderr, "{line}");
        wrote = true;
    }
    if wrote {
        let _ = stderr.flush();
    }
}

fn sleep_interruptibly(
    duration: Duration,
    interrupted: &AtomicBool,
    log_rx: &mpsc::Receiver<String>,
    stderr: &mut dyn Write,
) {
    let deadline = Instant::now() + duration;
    while !interrupted.load(Ordering::Acquire) {
        flush_worker_logs(log_rx, stderr);
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            break;
        }
        thread::sleep(rem.min(POLL_SLICE));
    }
}

pub struct PreparedServer {
    listener: TcpListener,
    local_addr: SocketAddr,
    route_path: String,
    cfg: ServeConfig,
    ack_window: Duration,
}

impl PreparedServer {
    pub fn bind(
        cfg: ServeConfig,
        room_services: &dyn RoomServices,
        interrupted: &AtomicBool,
    ) -> Result<Self, String> {
        if !cfg.listen.ip().is_loopback() {
            return Err("oms intake serve binds loopback addresses only".into());
        }
        validate_serve_room(&cfg, room_services, interrupted)?;
        let listener = TcpListener::bind(cfg.listen)
            .map_err(|e| format!("failed binding {}: {e}", cfg.listen))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("failed reading local listener address: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("failed configuring non-blocking listener: {e}"))?;
        let route_path = intake_path(cfg.source.name());
        Ok(Self {
            listener,
            local_addr,
            route_path,
            cfg,
            ack_window: ACK_WINDOW,
        })
    }

    pub fn with_ack_window(mut self, ack_window: Duration) -> Self {
        self.ack_window = ack_window;
        self
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn route_path(&self) -> &str {
        &self.route_path
    }

    pub fn serve_until_stopped(
        self,
        room_services: &dyn RoomServices,
        interrupted: &AtomicBool,
        stderr: &mut dyn Write,
    ) -> Result<(), String> {
        let poll_ts = Timespec::try_from(POLL_SLICE).expect("valid fixed poll duration");
        let source_name = self.cfg.source.name();
        let ack_window = self.ack_window;
        let worker_stop = AtomicBool::new(false);

        thread::scope(|scope| {
            let (tx, rx) = mpsc::sync_channel::<QueuedAck>(BG_QUEUE_CAP);
            let (log_tx, log_rx) = mpsc::channel::<String>();
            let source = Arc::clone(&self.cfg.source);
            let secrets = self.cfg.secrets.clone();
            let stop_ref = &worker_stop;

            let worker_handle = scope.spawn(move || {
                while !interrupted.load(Ordering::Acquire) && !stop_ref.load(Ordering::Acquire) {
                    let Ok(queued) = rx.recv() else {
                        break;
                    };
                    if interrupted.load(Ordering::Acquire) || stop_ref.load(Ordering::Acquire) {
                        break;
                    }
                    deliver_queued_ack(&*source, &secrets, &queued, interrupted, stop_ref, &log_tx);
                }
            });

            let mut loop_res = Ok(());
            let mut accept_backoff = AcceptBackoff::new();
            while !interrupted.load(Ordering::Acquire) {
                flush_worker_logs(&log_rx, stderr);
                let mut fds = [PollFd::new(&self.listener, PollFlags::IN)];
                match poll(&mut fds, Some(&poll_ts)) {
                    Ok(0) => continue,
                    Ok(_) => {}
                    Err(Errno::INTR) => continue,
                    Err(e) => {
                        loop_res = Err(format!("poll on intake listener failed: {e}"));
                        break;
                    }
                }

                loop {
                    if interrupted.load(Ordering::Acquire) {
                        break;
                    }
                    match self.listener.accept() {
                        Ok((stream, _peer)) => {
                            accept_backoff.on_success();
                            let accepted_at = Instant::now();
                            let outcome = handle_connection(
                                stream,
                                &self.route_path,
                                &self.cfg,
                                room_services,
                                interrupted,
                            );
                            let _ = writeln!(
                                stderr,
                                "intake serve: source={source_name} status={} session={} route={}",
                                outcome.status,
                                sanitize_log_id(outcome.session_id.as_deref()),
                                outcome.route.unwrap_or("-")
                            );
                            if let Some(ack) = outcome.post_response_ack {
                                let queued = QueuedAck {
                                    ack,
                                    deadline: accepted_at + ack_window,
                                };
                                if matches!(tx.try_send(queued), Err(mpsc::TrySendError::Full(_))) {
                                    let _ = writeln!(
                                        stderr,
                                        "intake serve: ack queue full; dropped outbound acknowledgement (source={source_name})"
                                    );
                                }
                            }
                            flush_worker_logs(&log_rx, stderr);
                            let _ = stderr.flush();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            let (delay, is_new_burst) = accept_backoff.on_error(&e);
                            if is_new_burst {
                                let errno = e
                                    .raw_os_error()
                                    .map_or_else(|| "-".to_owned(), |n| n.to_string());
                                let _ = writeln!(
                                    stderr,
                                    "intake serve: accept failed: source={source_name} kind={:?} errno={errno}; backing off",
                                    e.kind()
                                );
                                let _ = stderr.flush();
                            }
                            sleep_interruptibly(delay, interrupted, &log_rx, stderr);
                            break;
                        }
                    }
                }
            }

            worker_stop.store(true, Ordering::Release);
            drop(tx);
            let _ = worker_handle.join();
            flush_worker_logs(&log_rx, stderr);
            loop_res
        })
    }
}

/// In-process background server handle for fixture tests using `oms_command_room::LocalServices`.
pub struct ServerHandle {
    local_addr: SocketAddr,
    route_path: String,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Vec<u8>>>,
}

impl ServerHandle {
    pub fn spawn(cfg: ServeConfig) -> Result<Self, String> {
        Self::spawn_with_ack_window(cfg, ACK_WINDOW)
    }

    pub fn spawn_with_ack_window(cfg: ServeConfig, ack_window: Duration) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let prepared = PreparedServer::bind(cfg, &oms_command_room::LocalServices, &stop)?
            .with_ack_window(ack_window);
        let local_addr = prepared.local_addr();
        let route_path = prepared.route_path().to_owned();
        let worker_stop = Arc::clone(&stop);
        let join = thread::spawn(move || {
            let mut logs = Vec::new();
            let _ = prepared.serve_until_stopped(
                &oms_command_room::LocalServices,
                &worker_stop,
                &mut logs,
            );
            logs
        });
        Ok(Self {
            local_addr,
            route_path,
            stop,
            join: Some(join),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn intake_url(&self) -> String {
        format!("http://{}{}", self.local_addr, self.route_path)
    }

    pub fn shutdown(mut self) -> Vec<u8> {
        self.stop.store(true, Ordering::Release);
        self.join
            .take()
            .and_then(|j| j.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Default)]
struct RequestOutcome<'a> {
    status: u16,
    session_id: Option<String>,
    /// Matched route team, or `unrouted`, once a work request was parsed.
    route: Option<&'a str>,
    post_response_ack: Option<OutboundAck>,
}

impl RequestOutcome<'_> {
    fn status(status: u16, session_id: Option<String>) -> Self {
        Self {
            status,
            session_id,
            ..Self::default()
        }
    }
}

enum ParsedHttp {
    Request {
        headers: RequestHeaders,
        body: Vec<u8>,
    },
    Reject {
        status: u16,
        allow_post: bool,
    },
}

fn write_http_response(stream: &mut TcpStream, status: u16, allow_post: bool) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let extra = if allow_post { "Allow: POST\r\n" } else { "" };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn read_bounded_http(stream: &mut TcpStream, expected_path: &str) -> ParsedHttp {
    if stream.set_nonblocking(false).is_err() {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    }
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if pos > MAX_HEADER_BYTES {
                return ParsedHttp::Reject {
                    status: 413,
                    allow_post: false,
                };
            }
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return ParsedHttp::Reject {
                status: 413,
                allow_post: false,
            };
        }
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() || stream.set_read_timeout(Some(rem)).is_err() {
            return ParsedHttp::Reject {
                status: 408,
                allow_post: false,
            };
        }
        let max_next = (MAX_HEADER_BYTES + 4)
            .saturating_sub(buf.len())
            .max(1)
            .min(chunk.len());
        match stream.read(&mut chunk[..max_next]) {
            Ok(0) => {
                return ParsedHttp::Reject {
                    status: 400,
                    allow_post: false,
                };
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return ParsedHttp::Reject {
                    status: 408,
                    allow_post: false,
                };
            }
            Err(_) => {
                return ParsedHttp::Reject {
                    status: 400,
                    allow_post: false,
                };
            }
        }
    };

    let Ok(header_text) = std::str::from_utf8(&buf[..header_end]) else {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    };
    let mut lines = header_text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    }
    if target != expected_path {
        return ParsedHttp::Reject {
            status: 404,
            allow_post: false,
        };
    }
    if method != "POST" {
        return ParsedHttp::Reject {
            status: 405,
            allow_post: true,
        };
    }

    let mut content_length: Option<usize> = None;
    let mut headers = RequestHeaders::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((raw_name, raw_val)) = line.split_once(':') else {
            return ParsedHttp::Reject {
                status: 400,
                allow_post: false,
            };
        };
        if raw_name.is_empty() || raw_name.bytes().any(|b| b.is_ascii_whitespace()) {
            return ParsedHttp::Reject {
                status: 400,
                allow_post: false,
            };
        }
        let val = raw_val.trim_matches([' ', '\t']);
        if raw_name.eq_ignore_ascii_case("transfer-encoding") {
            return ParsedHttp::Reject {
                status: 400,
                allow_post: false,
            };
        }
        if raw_name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some()
                || val.is_empty()
                || !val.bytes().all(|b| b.is_ascii_digit())
            {
                return ParsedHttp::Reject {
                    status: 400,
                    allow_post: false,
                };
            }
            let Ok(parsed_len) = val.parse::<usize>() else {
                return ParsedHttp::Reject {
                    status: 413,
                    allow_post: false,
                };
            };
            if parsed_len > MAX_BODY_BYTES {
                return ParsedHttp::Reject {
                    status: 413,
                    allow_post: false,
                };
            }
            content_length = Some(parsed_len);
        } else {
            headers.push(raw_name.to_owned(), val.to_owned());
        }
    }

    let Some(expected_len) = content_length else {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    };

    let body_start = header_end + 4;
    let initial_body = &buf[body_start..];
    let mut body = Vec::with_capacity(expected_len);
    let copy_len = initial_body.len().min(expected_len);
    body.extend_from_slice(&initial_body[..copy_len]);

    while body.len() < expected_len {
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() || stream.set_read_timeout(Some(rem)).is_err() {
            return ParsedHttp::Reject {
                status: 408,
                allow_post: false,
            };
        }
        let want = (expected_len - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => {
                return ParsedHttp::Reject {
                    status: 400,
                    allow_post: false,
                };
            }
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return ParsedHttp::Reject {
                    status: 408,
                    allow_post: false,
                };
            }
            Err(_) => {
                return ParsedHttp::Reject {
                    status: 400,
                    allow_post: false,
                };
            }
        }
    }

    ParsedHttp::Request { headers, body }
}

fn handle_connection<'a>(
    mut stream: TcpStream,
    expected_path: &str,
    cfg: &'a ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> RequestOutcome<'a> {
    let (headers, raw_body) = match read_bounded_http(&mut stream, expected_path) {
        ParsedHttp::Reject { status, allow_post } => {
            write_http_response(&mut stream, status, allow_post);
            return RequestOutcome::status(status, None);
        }
        ParsedHttp::Request { headers, body } => (headers, body),
    };

    let outcome = process_verified_intake(&headers, &raw_body, cfg, room_services, interrupted);
    write_http_response(&mut stream, outcome.status, false);
    outcome
}

fn process_verified_intake<'a>(
    headers: &RequestHeaders,
    raw_body: &[u8],
    cfg: &'a ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> RequestOutcome<'a> {
    let sig_header = cfg.source.signature_header();
    if headers.count(sig_header) != 1 {
        return RequestOutcome::status(401, None);
    }
    let Some(sig_input) = cfg.source.signature_input(headers, raw_body) else {
        return RequestOutcome::status(401, None);
    };
    let Some(signing_secret) = cfg.secrets.get(sig_input.secret_env) else {
        return RequestOutcome::status(401, None);
    };
    if !verify_hmac_sha256_hex(
        signing_secret.as_bytes(),
        &sig_input.message,
        sig_input.signature_hex,
    ) {
        return RequestOutcome::status(401, None);
    }

    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(raw_body) else {
        return RequestOutcome::status(400, None);
    };

    let observed_session = cfg.source.observed_session_ref(&payload);

    let Some(ts_ms) = cfg.source.extract_timestamp_ms(headers, &payload) else {
        return RequestOutcome::status(400, observed_session);
    };
    if !within_timestamp_window_ms(current_unix_ms(), ts_ms, TIMESTAMP_WINDOW_MS) {
        return RequestOutcome::status(400, observed_session);
    }

    match cfg.source.check_binding(&cfg.binding_flags, &payload) {
        BindingDecision::Matched => {}
        BindingDecision::Ignore => {
            return RequestOutcome::status(200, None);
        }
        BindingDecision::Forbidden => {
            return RequestOutcome::status(403, observed_session);
        }
        BindingDecision::BadRequest => {
            return RequestOutcome::status(400, observed_session);
        }
    }

    let req = match cfg.source.parse_work_request(&payload) {
        PayloadDecision::Work(req) => req,
        PayloadDecision::Ignore => {
            return RequestOutcome::status(200, observed_session);
        }
        PayloadDecision::BadRequest => {
            return RequestOutcome::status(400, observed_session);
        }
    };

    let keys = cfg.source.routing_keys(&payload);
    let Some(route) = resolve(&cfg.routes, cfg.source.name(), &keys) else {
        return RequestOutcome {
            status: 200,
            route: Some("unrouted"),
            post_response_ack: cfg.source.unrouted_ack(&req),
            session_id: Some(req.session_ref),
        };
    };
    let Ok(delivery) = deliver_to_room(route, room_services, interrupted, &req) else {
        return RequestOutcome {
            status: 503,
            route: Some(&route.team),
            ..RequestOutcome::status(503, Some(req.session_ref))
        };
    };
    let post_response_ack = cfg.source.first_ack(&req, &route.room, delivery);

    RequestOutcome {
        status: 200,
        route: Some(&route.team),
        post_response_ack,
        session_id: Some(req.session_ref),
    }
}

pub fn is_room_content_refusal(msg: &str) -> bool {
    ROOM_CONTENT_REFUSAL_MESSAGES.contains(&msg)
}

fn deliver_to_room(
    cfg: &Route,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
    req: &WorkRequest,
) -> Result<RoomDelivery, ()> {
    let room = Room::open(
        &cfg.repo,
        RoomSession::from_env(),
        room_services,
        interrupted,
        false,
        false,
    )
    .map_err(|_| ())?;
    let existing = room.messages(&cfg.room).map_err(|_| ())?;
    let msg_id = &req.idempotency_key;
    if existing.iter().any(|m| m.text("id") == msg_id) {
        return Ok(RoomDelivery::Duplicate);
    }
    let text = format_work_request_room_text(req);

    match room.send(
        &cfg.room,
        RoomSend {
            sender: &cfg.participant,
            recipient: &cfg.to,
            text: &text,
            kind: "question",
            reply_to: None,
            message_id: Some(msg_id),
            linked: false,
            quiet: false,
        },
    ) {
        Ok(_) => Ok(RoomDelivery::AppendedNew),
        Err(e) if e.message == ROOM_DUPLICATE_CONFLICT_MESSAGE => Ok(RoomDelivery::Duplicate),
        Err(e) if is_room_content_refusal(&e.message) => Ok(RoomDelivery::Refused),
        Err(_) => Err(()),
    }
}
