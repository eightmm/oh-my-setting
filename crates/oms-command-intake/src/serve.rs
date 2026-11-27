//! Source-neutral loopback HTTP intake listener and Room `question` mapper.

use crate::crypto::{
    SecretStore, TIMESTAMP_WINDOW_MS, current_unix_ms, verify_hmac_sha256_hex,
    within_timestamp_window_ms,
};
use crate::model::{
    WorkRequest, format_work_request_room_text, intake_path, sanitize_log_id, validate_source_name,
};
use crate::source::{
    BindingDecision, OutboundAck, PayloadDecision, RequestHeaders, RoomDelivery, Source,
};
use oms_command_room::{Room, Send as RoomSend, Services as RoomServices, Session as RoomSession};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read as _, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8787";
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
const BG_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const BG_QUEUE_CAP: usize = 64;

#[derive(Clone)]
pub struct ServeConfig {
    pub source: Arc<dyn Source>,
    pub repo: PathBuf,
    pub room: String,
    pub participant: String,
    pub to: String,
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
    oms_command_room::identifier(&cfg.room, "room").map_err(|e| e.message)?;
    oms_command_room::identifier(&cfg.participant, "participant").map_err(|e| e.message)?;
    if cfg.to != "all" {
        oms_command_room::identifier(&cfg.to, "recipient").map_err(|e| e.message)?;
    }
    if cfg.participant == cfg.to {
        return Err("--participant and --to must name distinct Room participants".into());
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

    let room = Room::open(
        &cfg.repo,
        RoomSession::from_env(),
        room_services,
        interrupted,
        false,
        false,
    )
    .map_err(|e| format!("room unavailable: {}", e.message))?;
    let state = room
        .status(&cfg.room, false)
        .map_err(|e| format!("room '{}' unavailable: {}", cfg.room, e.message))?;
    if state.flag("closed") {
        return Err(format!("room '{}' is closed", cfg.room));
    }
    oms_command_room::participant(&state, &cfg.participant)
        .map_err(|e| format!("--participant '{}': {}", cfg.participant, e.message))?;
    if cfg.to != "all" {
        oms_command_room::participant(&state, &cfg.to)
            .map_err(|e| format!("--to '{}': {}", cfg.to, e.message))?;
    }
    Ok(())
}

pub struct PreparedServer {
    listener: TcpListener,
    local_addr: SocketAddr,
    route_path: String,
    cfg: ServeConfig,
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
        })
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
        let poll_ts =
            Timespec::try_from(Duration::from_millis(50)).expect("valid fixed poll duration");
        let source_name = self.cfg.source.name();
        let worker_stop = AtomicBool::new(false);

        thread::scope(|scope| {
            let (tx, rx) = mpsc::sync_channel::<OutboundAck>(BG_QUEUE_CAP);
            let source = Arc::clone(&self.cfg.source);
            let secrets = self.cfg.secrets.clone();
            let stop_ref = &worker_stop;

            scope.spawn(move || {
                while !interrupted.load(Ordering::Acquire) && !stop_ref.load(Ordering::Acquire) {
                    let Ok(ack) = rx.recv() else {
                        break;
                    };
                    if interrupted.load(Ordering::Acquire) || stop_ref.load(Ordering::Acquire) {
                        break;
                    }
                    let _ = source.deliver_ack(&secrets, &ack, BG_ACK_TIMEOUT, interrupted);
                }
            });

            let mut loop_res = Ok(());
            while !interrupted.load(Ordering::Acquire) {
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
                            let outcome = handle_connection(
                                stream,
                                &self.route_path,
                                &self.cfg,
                                room_services,
                                interrupted,
                            );
                            let _ = writeln!(
                                stderr,
                                "intake serve: source={source_name} status={} session={}",
                                outcome.status,
                                sanitize_log_id(outcome.session_id.as_deref())
                            );
                            if let Some(ack) = outcome.post_response_ack
                                && matches!(tx.try_send(ack), Err(mpsc::TrySendError::Full(_)))
                            {
                                let _ = writeln!(
                                    stderr,
                                    "intake serve: ack queue full; dropped outbound acknowledgement (source={source_name})"
                                );
                            }
                            let _ = stderr.flush();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            }

            worker_stop.store(true, Ordering::Release);
            drop(tx);
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
        let stop = Arc::new(AtomicBool::new(false));
        let prepared = PreparedServer::bind(cfg, &oms_command_room::LocalServices, &stop)?;
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

struct RequestOutcome {
    status: u16,
    session_id: Option<String>,
    post_response_ack: Option<OutboundAck>,
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

fn handle_connection(
    mut stream: TcpStream,
    expected_path: &str,
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> RequestOutcome {
    let (headers, raw_body) = match read_bounded_http(&mut stream, expected_path) {
        ParsedHttp::Reject { status, allow_post } => {
            write_http_response(&mut stream, status, allow_post);
            return RequestOutcome {
                status,
                session_id: None,
                post_response_ack: None,
            };
        }
        ParsedHttp::Request { headers, body } => (headers, body),
    };

    let outcome = process_verified_intake(&headers, &raw_body, cfg, room_services, interrupted);
    write_http_response(&mut stream, outcome.status, false);
    outcome
}

fn process_verified_intake(
    headers: &RequestHeaders,
    raw_body: &[u8],
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> RequestOutcome {
    let sig_header = cfg.source.signature_header();
    if headers.count(sig_header) != 1 {
        return RequestOutcome {
            status: 401,
            session_id: None,
            post_response_ack: None,
        };
    }
    let Some(sig_input) = cfg.source.signature_input(headers, raw_body) else {
        return RequestOutcome {
            status: 401,
            session_id: None,
            post_response_ack: None,
        };
    };
    let Some(signing_secret) = cfg.secrets.get(sig_input.secret_env) else {
        return RequestOutcome {
            status: 401,
            session_id: None,
            post_response_ack: None,
        };
    };
    if !verify_hmac_sha256_hex(
        signing_secret.as_bytes(),
        &sig_input.message,
        sig_input.signature_hex,
    ) {
        return RequestOutcome {
            status: 401,
            session_id: None,
            post_response_ack: None,
        };
    }

    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(raw_body) else {
        return RequestOutcome {
            status: 400,
            session_id: None,
            post_response_ack: None,
        };
    };

    let observed_session = cfg.source.observed_session_ref(&payload);

    let Some(ts_ms) = cfg.source.extract_timestamp_ms(headers, &payload) else {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_ack: None,
        };
    };
    if !within_timestamp_window_ms(current_unix_ms(), ts_ms, TIMESTAMP_WINDOW_MS) {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_ack: None,
        };
    }

    match cfg.source.check_binding(&cfg.binding_flags, &payload) {
        BindingDecision::Matched => {}
        BindingDecision::Ignore => {
            return RequestOutcome {
                status: 200,
                session_id: None,
                post_response_ack: None,
            };
        }
        BindingDecision::Forbidden => {
            return RequestOutcome {
                status: 403,
                session_id: observed_session,
                post_response_ack: None,
            };
        }
        BindingDecision::BadRequest => {
            return RequestOutcome {
                status: 400,
                session_id: observed_session,
                post_response_ack: None,
            };
        }
    }

    let req = match cfg.source.parse_work_request(&payload) {
        PayloadDecision::Work(req) => req,
        PayloadDecision::Ignore => {
            return RequestOutcome {
                status: 200,
                session_id: observed_session,
                post_response_ack: None,
            };
        }
        PayloadDecision::BadRequest => {
            return RequestOutcome {
                status: 400,
                session_id: observed_session,
                post_response_ack: None,
            };
        }
    };

    let Ok(delivery) = deliver_to_room(cfg, room_services, interrupted, &req) else {
        return RequestOutcome {
            status: 503,
            session_id: Some(req.session_ref),
            post_response_ack: None,
        };
    };
    let post_response_ack = cfg.source.first_ack(&req, &cfg.room, delivery);

    RequestOutcome {
        status: 200,
        session_id: Some(req.session_ref),
        post_response_ack,
    }
}

fn is_room_content_refusal(msg: &str) -> bool {
    matches!(
        msg,
        "turn looks sensitive; it would be replayed into other agents' prompts"
            | "room metadata contains sensitive-looking content or could not be checked"
            | "message needs 1..4000 UTF-8 bytes"
            | "--text or --text-file is required"
            | "room metadata cannot be encoded as UTF-8"
            | "room metadata is oversized"
    )
}

fn deliver_to_room(
    cfg: &ServeConfig,
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
        Err(e) if e.message == "message id conflicts with its existing content" => {
            Ok(RoomDelivery::Duplicate)
        }
        Err(e) if is_room_content_refusal(&e.message) => Ok(RoomDelivery::Refused),
        Err(_) => Err(()),
    }
}
