//! Loopback HTTP webhook receiver mapping Linear `AgentSessionEvent` webhooks into an OMS Room.

use crate::crypto::{SecretValue, verify_linear_signature};
use crate::model::{
    ActivityKind, CREATED_THOUGHT_CALLER_KEY, FIRST_THOUGHT_BODY, REFUSED_CONTENT_ERROR_BODY,
    created_message_id, derive_activity_id, format_created_room_text, format_prompted_room_text,
    prompted_message_id, sanitize_log_id, validate_external_id,
};
use crate::transport::{EndpointUrl, LinearClient};
use oms_command_room::{Room, Send as RoomSend, Services as RoomServices, Session as RoomSession};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use serde_json::Value;
use std::io::{Read as _, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8787";
pub const WEBHOOK_PATH: &str = "/linear/webhook";
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
pub const TIMESTAMP_WINDOW_MS: u64 = 60_000;
const BG_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(5);
const BG_QUEUE_CAP: usize = 64;

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub repo: PathBuf,
    pub room: String,
    pub participant: String,
    pub to: String,
    pub organization: String,
    pub app_user: String,
    pub listen: SocketAddr,
    pub signing_material: SecretValue,
    pub access_cred: SecretValue,
    pub api_url: EndpointUrl,
}

pub fn parse_loopback_listen(raw: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = raw
        .parse()
        .map_err(|_| format!("invalid --listen address '{raw}': expected LOOPBACK_IP:PORT"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing non-loopback --listen address '{addr}': oms linear serve binds loopback only"
        ));
    }
    Ok(addr)
}

pub fn validate_serve_room(
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> Result<(), String> {
    oms_command_room::identifier(&cfg.room, "room").map_err(|e| e.message)?;
    oms_command_room::identifier(&cfg.participant, "participant").map_err(|e| e.message)?;
    if cfg.to != "all" {
        oms_command_room::identifier(&cfg.to, "recipient").map_err(|e| e.message)?;
    }
    if cfg.participant == cfg.to {
        return Err("--participant and --to must name distinct Room participants".into());
    }
    validate_external_id(&cfg.organization, "organization")?;
    validate_external_id(&cfg.app_user, "app-user")?;

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

#[derive(Clone)]
struct BackgroundActivity {
    activity_id: String,
    session_id: String,
    kind: ActivityKind,
    body: String,
}

struct ActivityDispatcher {
    tx: Option<SyncSender<BackgroundActivity>>,
    worker: Option<JoinHandle<()>>,
}

impl ActivityDispatcher {
    fn spawn(client: LinearClient) -> Self {
        let (tx, rx) = mpsc::sync_channel::<BackgroundActivity>(BG_QUEUE_CAP);
        let worker = thread::spawn(move || {
            let never_stop = AtomicBool::new(false);
            while let Ok(task) = rx.recv() {
                let _ = client.create_activity(
                    &task.activity_id,
                    &task.session_id,
                    task.kind,
                    &task.body,
                    BG_ACTIVITY_TIMEOUT,
                    &never_stop,
                );
            }
        });
        Self {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    fn enqueue(&self, item: BackgroundActivity) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(item);
        }
    }

    fn shutdown(&mut self) {
        self.tx.take();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ActivityDispatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct PreparedServer {
    listener: TcpListener,
    local_addr: SocketAddr,
    cfg: ServeConfig,
}

impl PreparedServer {
    pub fn bind(
        cfg: ServeConfig,
        room_services: &dyn RoomServices,
        interrupted: &AtomicBool,
    ) -> Result<Self, String> {
        if !cfg.listen.ip().is_loopback() {
            return Err("oms linear serve binds loopback addresses only".into());
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
        Ok(Self {
            listener,
            local_addr,
            cfg,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn serve_until_stopped(
        self,
        room_services: &dyn RoomServices,
        interrupted: &AtomicBool,
        stderr: &mut dyn Write,
    ) -> Result<(), String> {
        let client = LinearClient::new(self.cfg.api_url.clone(), self.cfg.access_cred.clone());
        let mut bg = ActivityDispatcher::spawn(client);
        let poll_ts =
            Timespec::try_from(Duration::from_millis(50)).expect("valid fixed poll duration");

        while !interrupted.load(Ordering::Acquire) {
            let mut fds = [PollFd::new(&self.listener, PollFlags::IN)];
            match poll(&mut fds, Some(&poll_ts)) {
                Ok(0) => continue,
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(e) => {
                    bg.shutdown();
                    return Err(format!("poll on webhook listener failed: {e}"));
                }
            }

            loop {
                if interrupted.load(Ordering::Acquire) {
                    break;
                }
                match self.listener.accept() {
                    Ok((stream, _peer)) => {
                        let outcome =
                            handle_connection(stream, &self.cfg, room_services, interrupted, &bg);
                        let _ = writeln!(
                            stderr,
                            "linear serve: status={} session={}",
                            outcome.status,
                            sanitize_log_id(outcome.session_id.as_deref())
                        );
                        let _ = stderr.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }

        bg.shutdown();
        Ok(())
    }
}

/// In-process background server handle for fixture tests using `oms_command_room::LocalServices`.
pub struct ServerHandle {
    local_addr: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Vec<u8>>>,
}

impl ServerHandle {
    pub fn spawn(cfg: ServeConfig) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let prepared = PreparedServer::bind(cfg, &oms_command_room::LocalServices, &stop)?;
        let local_addr = prepared.local_addr();
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
            stop,
            join: Some(join),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn webhook_url(&self) -> String {
        format!("http://{}{WEBHOOK_PATH}", self.local_addr)
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
    post_response_activity: Option<BackgroundActivity>,
}

enum ParsedHttp {
    Request {
        signature: Option<String>,
        body: Vec<u8>,
    },
    Reject {
        status: u16,
        allow_post: bool,
    },
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
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

fn read_bounded_http(stream: &mut TcpStream) -> ParsedHttp {
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
    if target != WEBHOOK_PATH {
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
    let mut signature: Option<String> = None;
    let mut duplicate_sig = false;

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
        } else if raw_name.eq_ignore_ascii_case("linear-signature") {
            if signature.is_some() {
                duplicate_sig = true;
            } else {
                signature = Some(val.to_owned());
            }
        }
    }

    let Some(expected_len) = content_length else {
        return ParsedHttp::Reject {
            status: 400,
            allow_post: false,
        };
    };
    if duplicate_sig {
        return ParsedHttp::Reject {
            status: 401,
            allow_post: false,
        };
    }

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

    ParsedHttp::Request { signature, body }
}

fn handle_connection(
    mut stream: TcpStream,
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
    bg: &ActivityDispatcher,
) -> RequestOutcome {
    let (signature, raw_body) = match read_bounded_http(&mut stream) {
        ParsedHttp::Reject { status, allow_post } => {
            write_http_response(&mut stream, status, allow_post);
            return RequestOutcome {
                status,
                session_id: None,
                post_response_activity: None,
            };
        }
        ParsedHttp::Request { signature, body } => (signature, body),
    };

    let outcome = process_verified_webhook(
        signature.as_deref(),
        &raw_body,
        cfg,
        room_services,
        interrupted,
    );
    write_http_response(&mut stream, outcome.status, false);
    drop(stream);

    if let Some(activity) = outcome.post_response_activity.clone() {
        bg.enqueue(activity);
    }
    outcome
}

fn extract_webhook_timestamp_ms(val: &Value) -> Option<i64> {
    val.as_i64().or_else(|| {
        val.as_f64()
            .filter(|f| f.is_finite() && f.fract() == 0.0 && *f >= 0.0 && *f <= i64::MAX as f64)
            .map(|f| f as i64)
    })
}

fn extract_prompt_body(content: Option<&Value>) -> Option<String> {
    match content {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(map)) => {
            if let Some(body) = map.get("body").and_then(Value::as_str) {
                Some(body.to_owned())
            } else {
                serde_json::to_string(map).ok()
            }
        }
        Some(other) => serde_json::to_string(other).ok(),
    }
}

fn process_verified_webhook(
    signature: Option<&str>,
    raw_body: &[u8],
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
) -> RequestOutcome {
    let Some(sig) = signature else {
        return RequestOutcome {
            status: 401,
            session_id: None,
            post_response_activity: None,
        };
    };
    if !verify_linear_signature(cfg.signing_material.as_bytes(), raw_body, sig) {
        return RequestOutcome {
            status: 401,
            session_id: None,
            post_response_activity: None,
        };
    }

    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(raw_body) else {
        return RequestOutcome {
            status: 400,
            session_id: None,
            post_response_activity: None,
        };
    };

    let observed_session = payload
        .get("agentSession")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let Some(ts_ms) = payload
        .get("webhookTimestamp")
        .and_then(extract_webhook_timestamp_ms)
    else {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_activity: None,
        };
    };
    if current_unix_ms().abs_diff(ts_ms) > TIMESTAMP_WINDOW_MS {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_activity: None,
        };
    }

    let Some(event_type) = payload.get("type").and_then(Value::as_str) else {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_activity: None,
        };
    };
    if event_type != "AgentSessionEvent" {
        return RequestOutcome {
            status: 200,
            session_id: None,
            post_response_activity: None,
        };
    }

    let org_id = payload.get("organizationId").and_then(Value::as_str);
    let app_user_id = payload.get("appUserId").and_then(Value::as_str);
    if org_id != Some(cfg.organization.as_str()) || app_user_id != Some(cfg.app_user.as_str()) {
        return RequestOutcome {
            status: 403,
            session_id: observed_session,
            post_response_activity: None,
        };
    }

    let Some(action) = payload.get("action").and_then(Value::as_str) else {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_activity: None,
        };
    };
    if !matches!(action, "created" | "prompted") {
        return RequestOutcome {
            status: 200,
            session_id: observed_session,
            post_response_activity: None,
        };
    }

    let Some(session_obj) = payload.get("agentSession").and_then(Value::as_object) else {
        return RequestOutcome {
            status: 400,
            session_id: observed_session,
            post_response_activity: None,
        };
    };
    let Some(raw_session_id) = session_obj.get("id").and_then(Value::as_str) else {
        return RequestOutcome {
            status: 400,
            session_id: None,
            post_response_activity: None,
        };
    };
    if validate_external_id(raw_session_id, "agentSession.id").is_err() {
        return RequestOutcome {
            status: 400,
            session_id: None,
            post_response_activity: None,
        };
    }
    let session_id = raw_session_id.to_owned();

    if action == "created" {
        let prompt_ctx = match payload.get("promptContext") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => {
                return RequestOutcome {
                    status: 400,
                    session_id: Some(session_id),
                    post_response_activity: None,
                };
            }
        };
        let issue_identifier = session_obj
            .get("issue")
            .and_then(|i| i.get("identifier"))
            .and_then(Value::as_str);
        let session_url = session_obj.get("url").and_then(Value::as_str).or_else(|| {
            session_obj
                .get("issue")
                .and_then(|i| i.get("url"))
                .and_then(Value::as_str)
        });

        let msg_id = created_message_id(&session_id);
        let text = format_created_room_text(&session_id, issue_identifier, session_url, prompt_ctx);

        let deliver = deliver_to_room(cfg, room_services, interrupted, &msg_id, None, &text);
        let post_response_activity = match deliver {
            RoomDelivery::AppendedNew => Some(BackgroundActivity {
                activity_id: derive_activity_id(&session_id, &cfg.room, CREATED_THOUGHT_CALLER_KEY),
                session_id: session_id.clone(),
                kind: ActivityKind::Thought,
                body: FIRST_THOUGHT_BODY.to_owned(),
            }),
            RoomDelivery::Duplicate => None,
            RoomDelivery::Refused => Some(BackgroundActivity {
                activity_id: derive_activity_id(
                    &session_id,
                    &cfg.room,
                    &format!("error:refused:{msg_id}"),
                ),
                session_id: session_id.clone(),
                kind: ActivityKind::Error,
                body: REFUSED_CONTENT_ERROR_BODY.to_owned(),
            }),
        };

        return RequestOutcome {
            status: 200,
            session_id: Some(session_id),
            post_response_activity,
        };
    }

    let Some(activity_obj) = payload.get("agentActivity").and_then(Value::as_object) else {
        return RequestOutcome {
            status: 400,
            session_id: Some(session_id),
            post_response_activity: None,
        };
    };
    let Some(activity_id) = activity_obj.get("id").and_then(Value::as_str) else {
        return RequestOutcome {
            status: 400,
            session_id: Some(session_id),
            post_response_activity: None,
        };
    };
    if validate_external_id(activity_id, "agentActivity.id").is_err() {
        return RequestOutcome {
            status: 400,
            session_id: Some(session_id),
            post_response_activity: None,
        };
    }
    if let Some(act_sess) = activity_obj.get("agentSessionId").and_then(Value::as_str)
        && !act_sess.is_empty()
        && act_sess != session_id
    {
        return RequestOutcome {
            status: 400,
            session_id: Some(session_id),
            post_response_activity: None,
        };
    }
    let signal = match activity_obj.get("signal") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(_) => {
            return RequestOutcome {
                status: 400,
                session_id: Some(session_id),
                post_response_activity: None,
            };
        }
    };
    let prompt_body = extract_prompt_body(activity_obj.get("content"));
    let msg_id = prompted_message_id(activity_id);
    let created_id = created_message_id(&session_id);
    let text = format_prompted_room_text(&session_id, activity_id, signal, prompt_body.as_deref());

    let deliver = deliver_to_room(
        cfg,
        room_services,
        interrupted,
        &msg_id,
        Some(&created_id),
        &text,
    );
    let post_response_activity = match deliver {
        RoomDelivery::AppendedNew | RoomDelivery::Duplicate => None,
        RoomDelivery::Refused => Some(BackgroundActivity {
            activity_id: derive_activity_id(
                &session_id,
                &cfg.room,
                &format!("error:refused:{msg_id}"),
            ),
            session_id: session_id.clone(),
            kind: ActivityKind::Error,
            body: REFUSED_CONTENT_ERROR_BODY.to_owned(),
        }),
    };

    RequestOutcome {
        status: 200,
        session_id: Some(session_id),
        post_response_activity,
    }
}

enum RoomDelivery {
    AppendedNew,
    Duplicate,
    Refused,
}

fn deliver_to_room(
    cfg: &ServeConfig,
    room_services: &dyn RoomServices,
    interrupted: &AtomicBool,
    msg_id: &str,
    want_reply_to: Option<&str>,
    text: &str,
) -> RoomDelivery {
    let Ok(room) = Room::open(
        &cfg.repo,
        RoomSession::from_env(),
        room_services,
        interrupted,
        false,
        false,
    ) else {
        return RoomDelivery::Refused;
    };
    let Ok(existing) = room.messages(&cfg.room) else {
        return RoomDelivery::Refused;
    };
    let already_exists = existing.iter().any(|m| m.text("id") == msg_id);
    let reply_to = want_reply_to.filter(|parent_id| {
        existing.iter().any(|m| {
            m.text("id") == *parent_id
                && m.text("message_kind") == "question"
                && m.text("sender") == cfg.participant
                && m.text("recipient") == cfg.to
        })
    });

    match room.send(
        &cfg.room,
        RoomSend {
            sender: &cfg.participant,
            recipient: &cfg.to,
            text,
            kind: "question",
            reply_to,
            message_id: Some(msg_id),
            linked: false,
            quiet: false,
        },
    ) {
        Ok(_) if already_exists => RoomDelivery::Duplicate,
        Ok(_) => RoomDelivery::AppendedNew,
        Err(_) if already_exists => RoomDelivery::Duplicate,
        Err(_) => RoomDelivery::Refused,
    }
}
