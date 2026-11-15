//! App delivery reports transport submission separately from persisted output
//! and never claims that a desktop notification was displayed.
use crate::{Output, common::*};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FRAME_MAX: usize = 1024 * 1024;
pub(crate) fn state_path() -> PathBuf {
    env_path("XDG_STATE_HOME")
        .unwrap_or_else(|| home().join(".local/state"))
        .join("oh-my-setting/codex-notify.json")
}
pub(crate) fn turn_state_path(session_hash_hex: &str) -> PathBuf {
    state_path()
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join(format!("codex-notify-sessions/{session_hash_hex}.json"))
}
pub fn socket_path() -> PathBuf {
    env_path("OMS_CODEX_NOTIFY_SOCK").unwrap_or_else(|| {
        env_path("CODEX_HOME")
            .unwrap_or_else(|| home().join(".codex"))
            .join("app-server-control/app-server-control.sock")
    })
}
pub fn thread_id() -> String {
    text(&load(&state_path()), "thread_id").into()
}
fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0; N];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|_| "native random source unavailable")?;
    Ok(b)
}
fn base64(raw: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for group in raw.chunks(3) {
        let a = group[0] as usize;
        let b = group.get(1).copied().unwrap_or(0) as usize;
        let c = group.get(2).copied().unwrap_or(0) as usize;
        out.push(alphabet[a >> 2] as char);
        out.push(alphabet[(a & 3) * 16 + (b >> 4)] as char);
        out.push(if group.len() > 1 {
            alphabet[(b & 15) * 4 + (c >> 6)] as char
        } else {
            '='
        });
        out.push(if group.len() > 2 {
            alphabet[c & 63] as char
        } else {
            '='
        });
    }
    out
}
fn connect(path: &Path, deadline: Instant) -> Result<UnixStream, String> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let fd = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|_| "socket unavailable")?;
    let addr = SocketAddrUnix::new(path).map_err(|_| "invalid socket path")?;
    loop {
        match rustix::net::connect(&fd, &addr) {
            Ok(()) | Err(rustix::io::Errno::ISCONN) => break,
            Err(e)
                if [
                    e == rustix::io::Errno::AGAIN,
                    e == rustix::io::Errno::INPROGRESS,
                    e == rustix::io::Errno::ALREADY,
                ]
                .contains(&true) =>
            {
                if Instant::now() >= deadline {
                    return Err("timeout".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err("socket connection failed".into()),
        }
    }
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(false)
        .map_err(|_| "socket setup failed")?;
    Ok(stream)
}
struct Client {
    sock: UnixStream,
    deadline: Instant,
    next: u64,
    events: VecDeque<Value>,
}
impl Client {
    fn new(path: &Path, deadline: Instant) -> Result<Self, String> {
        let sock = connect(path, deadline)?;
        let mut client = Self {
            sock,
            deadline,
            next: 0,
            events: VecDeque::new(),
        };
        let key = base64(&random::<16>()?);
        client.timeout()?;
        client.sock.write_all(format!("GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).map_err(|_|"websocket write failed")?;
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() >= 16384 {
                return Err("websocket header too large".into());
            }
            header.push(client.take(1)?[0]);
        }
        if !header
            .split(|b| *b == b'\n')
            .next()
            .unwrap_or_default()
            .windows(5)
            .any(|s| s == b" 101 ")
        {
            return Err("websocket upgrade refused".into());
        }
        Ok(client)
    }
    fn timeout(&self) -> Result<(), String> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or("timeout")?;
        if remaining.is_zero() {
            return Err("timeout".into());
        }
        self.sock
            .set_read_timeout(Some(remaining))
            .and_then(|_| self.sock.set_write_timeout(Some(remaining)))
            .map_err(|_| "timeout setup failed".into())
    }
    fn take(&mut self, size: usize) -> Result<Vec<u8>, String> {
        if size > FRAME_MAX {
            return Err("websocket frame too large".into());
        }
        let mut out = vec![0; size];
        self.timeout()?;
        self.sock.read_exact(&mut out).map_err(|e| {
            if [
                e.kind() == std::io::ErrorKind::TimedOut,
                e.kind() == std::io::ErrorKind::WouldBlock,
            ]
            .contains(&true)
            {
                "timeout"
            } else {
                "websocket read failed"
            }
        })?;
        Ok(out)
    }
    fn send(&mut self, opcode: u8, data: &[u8]) -> Result<(), String> {
        if data.len() > FRAME_MAX {
            return Err("websocket message too large".into());
        }
        let n = data.len();
        let mut bytes = vec![0x80 | opcode];
        if n < 126 {
            bytes.push(0x80 | n as u8);
        } else if n < 65536 {
            bytes.push(0xfe);
            bytes.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            bytes.push(0xff);
            bytes.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask = random::<4>()?;
        bytes.extend_from_slice(&mask);
        bytes.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.timeout()?;
        self.sock
            .write_all(&bytes)
            .map_err(|_| "websocket write failed".into())
    }
    fn message(&mut self) -> Result<Value, String> {
        let mut parts = Vec::new();
        let mut started = false;
        let mut frames = 0;
        loop {
            frames += 1;
            if frames > 1024 {
                return Err("too many websocket frames".into());
            }
            let head = self.take(2)?;
            let opcode = head[0] & 15;
            if head[0] & 0x70 != 0 || head[1] & 0x80 != 0 {
                return Err("invalid websocket frame".into());
            }
            let mut size = u64::from(head[1] & 127);
            if size == 126 {
                size = u64::from(u16::from_be_bytes(self.take(2)?.try_into().unwrap()));
            } else if size == 127 {
                size = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
            }
            if size > FRAME_MAX as u64 || parts.len() as u64 + size > FRAME_MAX as u64 {
                return Err("websocket frame too large".into());
            }
            let data = self.take(size as usize)?;
            if opcode == 8 {
                return Err("app-server closed connection".into());
            }
            if opcode == 9 {
                if size > 125 || head[0] & 128 == 0 {
                    return Err("invalid ping".into());
                }
                self.send(10, &data)?;
                continue;
            }
            if opcode == 10 {
                continue;
            }
            if opcode != 0 && opcode != 1 || opcode == 0 && !started || opcode == 1 && started {
                return Err("invalid websocket continuation".into());
            }
            started = true;
            parts.extend(data);
            if head[0] & 128 != 0 {
                return strict_document(&parts);
            }
        }
    }
    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(
            1,
            &serde_json::to_vec(&json!({"jsonrpc":"2.0","method":method,"params":params}))
                .map_err(|_| "invalid RPC")?,
        )
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let request = json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params});
        self.send(1, &serde_json::to_vec(&request).map_err(|_| "invalid RPC")?)?;
        for _ in 0..256 {
            let reply = self.message()?;
            if reply.get("method").is_some() {
                if self.events.len() >= 128 {
                    return Err("too many notification events".into());
                }
                self.events.push_back(reply);
                continue;
            }
            if reply["id"].as_u64() == Some(self.next) {
                if let Some(error) = reply.get("error") {
                    return Err(format!("rpc_rejection:{error}"));
                }
                return Ok(reply.get("result").cloned().unwrap_or_else(|| json!({})));
            }
        }
        Err("too many RPC responses".into())
    }
    fn event(&mut self) -> Result<Value, String> {
        self.events.pop_front().map_or_else(|| self.message(), Ok)
    }
}
pub fn safe_message(raw: &str, repo: &Path, maximum: usize) -> String {
    let stripped = cached_regex!("\x1b\\[[0-?]*[ -/]*[@-~]").replace_all(raw, "");
    let stripped = stripped
        .chars()
        .map(|c| if c == '\n' || !control(c) { c } else { ' ' })
        .collect::<String>();
    let stripped =
        cached_regex!(r"(?i)\b(?:passw(?:or)?d|to[k]en|api[_-]?key|sec[r]et)\s*[:=]\s*\S+")
            .replace_all(&stripped, "[redacted]");
    let stripped = cached_regex!(r"(?i)\bBearer\s+\S+").replace_all(&stripped, "[redacted]");
    let mut stripped = stripped.into_owned();
    for p in [home(), repo.canonicalize().unwrap_or_else(|_| repo.into())] {
        if p != Path::new("/") {
            stripped = stripped.replace(p.to_string_lossy().as_ref(), "[path]");
        }
    }
    // Normalize path-bearing notification data before its final byte bound.
    let stripped =
        cached_regex!(r"(?:^|\s)/(?:[^\s/]+/)*[^\s/]+").replace_all(&stripped, " [path]");
    let lines = stripped
        .lines()
        .map(|s| safe_text(s, maximum.max(2400)))
        .collect::<Vec<_>>();
    let message = lines.join("\n").trim_end().to_owned() + "\n";
    if message.len() <= maximum {
        return message;
    }
    let last = lines
        .iter()
        .rfind(|s| !s.trim().is_empty())
        .map(String::as_str)
        .unwrap_or("");
    let ending = format!("…\n{last}\n");
    if ending.len() < maximum {
        let body = lines[..lines.len().saturating_sub(1)].join("\n");
        let prefix = clip(&body, maximum - ending.len() - 1);
        format!("{}\n{ending}", prefix.trim_end())
    } else {
        format!("{}\n", clip(&message, maximum.saturating_sub(1)).trim_end())
    }
}
fn archived(v: &Value) -> bool {
    v["archived"] == true
        || v["isArchived"] == true
        || v.get("archivedAt").is_some_and(|v| !v.is_null())
        || v["status"] == "archived"
}
fn command_matches(raw: &Value, expected: &str) -> bool {
    let Some(words) = raw.as_str().and_then(shlex::split) else {
        return false;
    };
    let wanted = shlex::split(expected);
    if Some(words.clone()) == wanted {
        return true;
    }
    words.len() == 3
        && Path::new(&words[0])
            .file_name()
            .is_some_and(|n| n == "bash" || n == "sh" || n == "zsh")
        && ["-c", "-lc"].contains(&words[1].as_str())
        && shlex::split(&words[2]) == wanted
}
fn quote(s: &str) -> String {
    shlex::try_quote(s)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| "''".into())
}
#[allow(clippy::too_many_arguments)]
fn receipt(
    status: &str,
    tid: &str,
    turn: &str,
    ack: bool,
    completed: bool,
    persisted: bool,
    submitted: bool,
    unknown: bool,
) -> Value {
    json!({"schema":1,"status":status,"reason":if status=="persisted"{""}else{status},"thread_id":tid,"turn_id":turn,"acknowledged":ack,"completed":completed,"persisted":persisted,"submitted":submitted,"delivery_unknown":unknown,"desktop_notification_observed":false})
}
fn persisted(
    client: &mut Client,
    tid: &str,
    turn: &str,
    item_id: &str,
    command: &str,
    expected: &str,
) -> Result<bool, String> {
    let match_item = |v: &Value| {
        text(v, "id") == item_id
            && text(v, "type") == "commandExecution"
            && command_matches(&v["command"], command)
            && v["exitCode"] == 0
            && text(v, "aggregatedOutput") == expected
    };
    match client.call(
        "thread/items/list",
        json!({"threadId":tid,"turnId":turn,"limit":32}),
    ) {
        Ok(page) => Ok(page["data"]
            .as_array()
            .filter(|a| a.len() <= 32)
            .is_some_and(|a| {
                a.iter().any(|e| {
                    text(e, "turnId") == turn
                        && e["item"]["status"] == "completed"
                        && match_item(&e["item"])
                })
            })),
        Err(e)
            if ["unsupported", "method not found", "-32601"]
                .iter()
                .any(|p| e.to_lowercase().contains(p)) =>
        {
            let row = client.call("thread/read", json!({"threadId":tid,"includeTurns":true}))?;
            let thread = &row["thread"];
            Ok(text(thread, "id") == tid
                && !archived(thread)
                && thread["turns"]
                    .as_array()
                    .filter(|a| a.len() <= 128)
                    .is_some_and(|a| {
                        a.iter().filter(|t| text(t, "id") == turn).any(|t| {
                            t["items"]
                                .as_array()
                                .filter(|a| a.len() <= 128)
                                .is_some_and(|a| a.iter().any(match_item))
                        })
                    }))
        }
        Err(e) => Err(e),
    }
}
/// Sends only to the saved receiver. A timeout after submission is uncertain,
/// and only matching stored command output earns `persisted: true`.
pub fn deliver_codex(repo: &Path, text_value: &str, timeout: f64) -> Value {
    let tid = thread_id();
    let simple = |s| receipt(s, &tid, "", false, false, false, false, false);
    if env("OMS_CODEX_NOTIFY") == "0" {
        return simple("disabled");
    }
    if tid.is_empty() || !socket_path().exists() {
        return simple("absent");
    }
    if !timeout.is_finite() || timeout <= 0. || timeout > 120. {
        return simple("invalid_timeout");
    }
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let (mut submitted, mut ack, mut completed) = (false, false, false);
    let mut turn = String::new();
    let mut message_path = None;
    let attempt = (|| -> Result<&str, String> {
        let _guard = lock(&state_path().with_extension("lock"), repo)?;
        let mut client = Client::new(&socket_path(), deadline)?;
        client.call("initialize",json!({"clientInfo":{"name":"oms-codex-notify","version":"1"},"capabilities":{"experimentalApi":true}}))?;
        client.notify("initialized", json!({}))?;
        let before = match client.call("thread/read", json!({"threadId":tid,"includeTurns":false}))
        {
            Ok(v) => v,
            Err(e)
                if ["no rollout", "not found"]
                    .iter()
                    .any(|p| e.to_lowercase().contains(p)) =>
            {
                return Ok("absent");
            }
            Err(e) => return Err(e),
        };
        if text(&before["thread"], "id") != tid {
            return Ok("absent");
        }
        if archived(&before["thread"]) {
            return Ok("archived");
        }
        let resumed = match client.call(
            "thread/resume",
            json!({"threadId":tid,"excludeTurns":true,"config":{"features":{"hooks":false}}}),
        ) {
            Ok(v) => v,
            Err(e) if e.to_lowercase().contains("archived") => return Ok("archived"),
            Err(e) => return Err(e),
        };
        if resumed["thread"]["status"]["type"] == "active" {
            return Ok("busy");
        }
        let path = state_path().with_file_name(format!(
            "codex-notify.{}.{}.msg",
            std::process::id(),
            hash(random::<16>()?)[..16].to_owned()
        ));
        let body = safe_message(text_value, repo, 2399);
        mutate(&path, INPUT_MAX, false, |_| {
            Ok((body.as_bytes().to_vec(), ()))
        })?;
        message_path = Some(path.clone());
        let command = format!("cat {}", quote(&path.to_string_lossy()));
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 10000) as u64;
        submitted = true;
        let response = client.call(
            "thread/shellCommand",
            json!({"threadId":tid,"timeoutMs":left,"command":command}),
        )?;
        ack = true;
        turn = text(&response, "turnId").into();
        let mut item_id = String::new();
        let (mut item_ok, mut turn_done) = (false, false);
        for _ in 0..128 {
            let e = client.event()?;
            let p = &e["params"];
            let event_turn = first(p, &["turnId"]);
            let event_turn = if event_turn.is_empty() {
                text(&p["turn"], "id")
            } else {
                event_turn
            };
            if text(p, "threadId") != tid || event_turn.is_empty() {
                continue;
            }
            let item = &p["item"];
            match text(&e, "method") {
                "item/started"
                    if text(item, "type") == "commandExecution"
                        && command_matches(&item["command"], &command)
                        && (turn.is_empty() || turn == event_turn) =>
                {
                    turn = event_turn.into();
                    item_id = text(item, "id").into();
                }
                "item/completed" => {
                    if turn.is_empty() && command_matches(&item["command"], &command) {
                        turn = event_turn.into();
                        item_id = text(item, "id").into();
                    }
                    if event_turn == turn && !item_id.is_empty() && text(item, "id") == item_id {
                        if text(item, "type") != "commandExecution" || item["exitCode"] != 0 {
                            return Ok("failed_command");
                        }
                        item_ok = true;
                    }
                }
                "turn/completed" if event_turn == turn => {
                    let status = text(&p["turn"], "status");
                    if !status.is_empty() && status != "completed" {
                        return Ok("failed_command");
                    }
                    turn_done = true;
                }
                _ => {}
            }
            if item_ok && turn_done {
                completed = true;
                return Ok(
                    if persisted(&mut client, &tid, &turn, &item_id, &command, &body)? {
                        "persisted"
                    } else {
                        "persistence_failure"
                    },
                );
            }
        }
        Err("too many notification events".into())
    })();
    if let Some(path) = message_path {
        let _ = fs::remove_file(path);
    }
    let (status, unknown) = match attempt {
        Ok(s) => (s, submitted && s == "persistence_failure"),
        Err(ref e) if e == "timeout" => ("timeout", submitted),
        Err(ref e) if e.starts_with("rpc_rejection:") => ("rpc_error", ack),
        Err(_) => ("rpc_error", submitted),
    };
    receipt(
        status,
        &tid,
        &turn,
        ack,
        completed,
        status == "persisted",
        submitted,
        unknown,
    )
}
fn owned_path(
    path: &Path,
    directory: bool,
    private: bool,
    protected: bool,
) -> Result<fs::Metadata, String> {
    if !path.is_absolute() {
        return Err("unsupported inbox path".into());
    }
    let own = rustix::process::getuid().as_raw();
    let original = fs::symlink_metadata(path).map_err(|_| "inbox unavailable")?;
    for part in path.ancestors() {
        let m = fs::symlink_metadata(part).map_err(|_| "inbox ancestry unavailable")?;
        if m.file_type().is_symlink() {
            return Err("symlinked inbox path".into());
        }
        if part == path
            && (m.uid() != own || directory && !m.is_dir() || private && m.mode() & 0o077 != 0)
        {
            return Err("unsafe inbox owner or mode".into());
        }
        if m.mode() & 0o022 != 0
            && !(part == path && protected)
            && !(m.is_dir() && m.mode() & 0o1000 != 0 && m.uid() == 0)
        {
            return Err("writable inbox ancestry".into());
        }
    }
    Ok(original)
}
fn registry() -> Result<Vec<Value>, String> {
    let base = env_path("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| home().join(".claude"))
        .join("sessions");
    owned_path(&base, true, true, false)?;
    let mut paths = Vec::new();
    for (i, e) in fs::read_dir(&base)
        .map_err(|_| "native registry unavailable")?
        .enumerate()
    {
        if i >= 1024 {
            return Err("native registry discovery is incomplete".into());
        }
        let e = e.map_err(|_| "native registry discovery is incomplete")?;
        if e.file_name().to_string_lossy().ends_with(".json") {
            paths.push(e.path());
            if paths.len() > 128 {
                return Err("native registry discovery is incomplete".into());
            }
        }
    }
    paths.sort();
    let mut rows = Vec::new();
    for path in paths {
        let Ok(before) = owned_path(&path, false, false, true) else {
            continue;
        };
        if !before.is_file() || before.nlink() != 1 || before.len() > 16384 {
            continue;
        }
        let Ok(raw) = read(&path, 16384) else {
            continue;
        };
        let Ok(after) = fs::symlink_metadata(&path) else {
            continue;
        };
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            continue;
        }
        let Some(row) = document(&raw) else {
            continue;
        };
        if row["peerProtocol"].as_i64() != Some(1)
            || text(&row, "sessionId").is_empty()
            || row["pid"]
                .as_i64()
                .is_none_or(|p| p <= 0 || p > i32::MAX as i64)
            || row["messagingSocketPath"].as_str().is_none()
        {
            continue;
        }
        if !oms_command_operations::lock::pid_alive(row["pid"].as_i64().unwrap()) {
            continue;
        }
        rows.push(row);
    }
    Ok(rows)
}
pub fn current_claude_session() -> Result<String, String> {
    let own = env("CLAUDE_CODE_MESSAGING_SOCKET");
    if own.is_empty() {
        return Err("current Claude session has no exported inbox".into());
    }
    let rows = registry()?;
    let matches = rows
        .iter()
        .filter(|r| text(r, "messagingSocketPath") == own)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err("current Claude session is absent or ambiguous".into());
    }
    Ok(text(matches[0], "sessionId").into())
}
/// Posting to Claude can start a paid turn and requires explicit wakeup authority.
pub fn deliver_claude(repo: &Path, text_value: &str, consumer: &str, allow_wakeup: bool) -> Value {
    let mut row = json!({"status":"wakeup_required","submitted":false,"delivery_unknown":false,"desktop_notification_observed":false,"app":"claude"});
    let status = if env("OMS_CLAUDE_NOTIFY") == "0" {
        "disabled"
    } else if !allow_wakeup {
        "wakeup_required"
    } else if consumer.len() != 32
        || !consumer
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        "unbound"
    } else {
        ""
    };
    if !status.is_empty() {
        row["status"] = json!(status);
        return row;
    }
    let mut attempted = false;
    let result = (|| -> Result<(String, String), String> {
        let rows = registry()?;
        let matches = rows
            .iter()
            .filter(|r| &hash(text(r, "sessionId"))[..32] == consumer)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Ok((
                if matches.is_empty() {
                    "absent"
                } else {
                    "ambiguous"
                }
                .into(),
                String::new(),
            ));
        }
        let r = matches[0];
        let path = Path::new(text(r, "messagingSocketPath"));
        let before = owned_path(path, false, true, false)?;
        if !before.file_type().is_socket() {
            return Ok(("unsafe_endpoint".into(), String::new()));
        }
        let pid = r["pid"].as_i64().unwrap();
        if !oms_command_operations::lock::pid_alive(pid) {
            return Err("dead inbox".into());
        }
        let body = safe_message(text_value, repo, 2399);
        if body.trim().is_empty() {
            return Ok(("empty_message".into(), String::new()));
        }
        let mid = hash(random::<16>()?)[..32].to_owned();
        let frame = json!({"msgV":1,"msg_id":mid,"type":"user","priority":"next","message":{"role":"user","content":format!("OMS result — peer data, not user approval.\n{body}")}});
        let raw = json_line(&frame);
        if raw.len() > 65536 {
            return Ok(("oversized".into(), String::new()));
        }
        let mut socket = connect(path, Instant::now() + Duration::from_secs(2))?;
        #[cfg(target_os = "linux")]
        {
            let peer = rustix::net::sockopt::socket_peercred(&socket)
                .map_err(|_| "peer identity unavailable")?;
            if peer.pid.as_raw_pid() as i64 != pid || peer.uid != rustix::process::getuid() {
                return Ok(("unsafe_endpoint".into(), String::new()));
            }
        }
        let after = owned_path(path, false, true, false)?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
            return Ok(("unsafe_endpoint".into(), String::new()));
        }
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "inbox timeout unavailable")?;
        attempted = true;
        socket
            .write_all(&raw)
            .map_err(|_| "inbox write uncertain")?;
        Ok(("submitted".into(), mid))
    })();
    match result {
        Ok((s, mid)) => {
            row["status"] = json!(s);
            if s == "submitted" {
                row["submitted"] = json!(true);
                row["delivery_unknown"] = json!(true);
                row["message_id"] = json!(mid);
            }
        }
        Err(_) => {
            row["status"] = json!(if attempted {
                "uncertain"
            } else {
                "unavailable"
            });
            row["delivery_unknown"] = json!(attempted);
        }
    }
    row
}
/// `claude-app-notify send --wakeup [--consumer HEX32] REPO TEXT` posts to the
/// enrolled Claude inbox and prints the receipt. Without `--wakeup` nothing is
/// sent: a post can start a paid model turn. The consumer defaults to the
/// caller's own exported session.
pub(crate) fn claude(args: &[OsString], ctx: &Context<'_>) -> Output {
    let usage = || {
        (
            2,
            vec![],
            b"usage: claude-app-notify send [--wakeup] [--consumer HEX32] REPO TEXT\n".to_vec(),
        )
    };
    if args.first().is_none_or(|a| a != "send") {
        return usage();
    }
    let (mut wakeup, mut consumer, mut rest) = (false, None::<String>, Vec::new());
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let Some(t) = a.to_str() else {
            if a.as_encoded_bytes().starts_with(b"-") && a.len() > 1 {
                return usage();
            }
            rest.push(a);
            continue;
        };
        match t {
            "--" => {
                rest.extend(it.by_ref());
                break;
            }
            "--wakeup" => wakeup = true,
            "--consumer" => match it.next().and_then(|v| v.to_str()) {
                Some(v) if !v.starts_with('-') => consumer = Some(v.to_owned()),
                _ => return usage(),
            },
            _ if t.starts_with("--consumer=") => consumer = Some(t[11..].to_owned()),
            _ if t.starts_with('-') && t.len() > 1 => return usage(),
            _ => rest.push(a),
        }
    }
    if rest.len() != 2 {
        return usage();
    }
    let repo = absolute(ctx.cwd, Path::new(rest[0]));
    // Session discovery scans the live registry; only a wakeup needs it.
    let consumer = match consumer {
        Some(c) => c,
        None if wakeup && env("OMS_CLAUDE_NOTIFY") != "0" => current_claude_session()
            .map(|s| hash(s)[..32].to_owned())
            .unwrap_or_default(),
        None => String::new(),
    };
    let receipt = deliver_claude(&repo, &rest[1].to_string_lossy(), &consumer, wakeup);
    out(json_line(&receipt))
}
pub(crate) fn run(args: &[OsString], ctx: &Context<'_>) -> Output {
    if args.len() != 3 || args[0] != "send" {
        return (
            2,
            vec![],
            b"usage: codex-app-notify send REPO TEXT\n".to_vec(),
        );
    }
    let repo = absolute(ctx.cwd, Path::new(&args[1]));
    if !socket_path().exists() {
        return empty();
    }
    let _ = (|| -> Result<(), String> {
        let _guard = lock(&state_path().with_extension("lock"), &repo)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut client = Client::new(&socket_path(), deadline)?;
        client.call(
            "initialize",
            json!({"clientInfo":{"name":"oms-codex-notify","version":"1"}}),
        )?;
        client.notify("initialized", json!({}))?;
        let mut tid = thread_id();
        if tid.is_empty() {
            let started = client.call(
                "thread/start",
                json!({"cwd":repo,"config":{"features":{"hooks":false}}}),
            )?;
            tid = text(&started["thread"], "id").into();
            if tid.is_empty() {
                return Err("thread start returned no id".into());
            }
            client.call(
                "thread/name/set",
                json!({"threadId":tid,"name":"Claude 알림"}),
            )?;
            write(&state_path(), &json!({"schema":1,"thread_id":tid}))?;
        } else {
            client.call(
                "thread/resume",
                json!({"threadId":tid,"excludeTurns":true,"config":{"features":{"hooks":false}}}),
            )?;
        }
        let path = state_path().with_file_name(format!(
            "codex-notify.{}.{}.msg",
            std::process::id(),
            hash(random::<16>()?)[..16].to_owned()
        ));
        let body = args[2].to_string_lossy();
        let body = safe_message(&body, &repo, 2399);
        mutate(&path, INPUT_MAX, false, |_| {
            Ok((body.as_bytes().to_vec(), ()))
        })?;
        let sent = (|| -> Result<(), String> {
            client.call("thread/shellCommand",json!({"threadId":tid,"timeoutMs":10000,"command":format!("cat {}",quote(&path.to_string_lossy()))}))?;
            for _ in 0..128 {
                if client.event()?["method"] == "turn/completed" {
                    return Ok(());
                }
            }
            Err("too many notification events".into())
        })();
        let _ = fs::remove_file(path);
        sent
    })();
    empty()
}
pub fn sequence(mode: &str, title: &str, text_value: &str) -> Vec<u8> {
    let clean = |s: &str, semi: bool| {
        s.chars()
            .map(|c| {
                if c <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&c) || semi && c == ';' {
                    ' '
                } else {
                    c
                }
            })
            .collect::<String>()
    };
    let title = clean(title, true);
    let body = clean(text_value, false);
    match mode {
        "777" => format!("\x1b]777;notify;{title};{body}\x07"),
        "99" => format!("\x1b]99;;{body}\x1b\\"),
        _ => format!("\x1b]9;{body}\x07"),
    }
    .into_bytes()
}
fn walk(directory: &Path, limit: usize) -> Vec<PathBuf> {
    let mut stack = vec![directory.to_owned()];
    let mut out = Vec::new();
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            visited += 1;
            if visited > limit {
                return out;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}
pub(crate) fn rollout_files() -> Vec<PathBuf> {
    walk(
        &env_path("CODEX_HOME")
            .or_else(|| env_path("OMS_CODEX_HOME"))
            .unwrap_or_else(|| home().join(".codex"))
            .join("sessions"),
        20000,
    )
}
pub(crate) fn turn(args: &[OsString], ctx: &Context<'_>) -> Output {
    if !on("OMS_TERMINAL_NOTIFY") || child() {
        return empty();
    }
    let Some(v) = args.last().and_then(|s| document(s.as_encoded_bytes())) else {
        return empty();
    };
    if v["type"] != "agent-turn-complete" || text(&v, "last-assistant-message").is_empty() {
        return empty();
    }
    let thread = text(&v, "thread-id");
    if !thread.is_empty() && thread == thread_id() {
        return empty();
    }
    if !thread.is_empty() {
        for path in rollout_files() {
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(thread))
                && let Ok(raw) = read(&path, 64 * INPUT_MAX)
                && let Some(row) = raw.split(|b| *b == b'\n').next().and_then(document)
            {
                let source = &row["payload"]["source"];
                if source == "exec" || source.get("subagent").is_some() {
                    return empty();
                }
                break;
            }
        }
    }
    let cwd = text(&v, "cwd");
    let title = format!(
        "🔔 Codex 작업 완료 · {}",
        Path::new(cwd)
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("codex"))
            .to_string_lossy()
    );
    let body = safe_text(
        &format!("{title}: {}", text(&v, "last-assistant-message")),
        240,
    );
    let data = sequence(
        if env("OMS_TERMINAL_NOTIFY_OSC") == "777" {
            "777"
        } else {
            "9"
        },
        &title,
        &body,
    );
    let tmux = |args: &[&str]| -> Vec<Vec<String>> {
        let mut argv = vec![OsString::from("tmux")];
        argv.extend(args.iter().map(OsString::from));
        invocation(argv, None, ctx.cwd, Duration::from_secs(3), STATE_MAX, ctx)
            .ok()
            .filter(|p| p.complete)
            .map(|p| {
                String::from_utf8_lossy(&p.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.split('\t').map(str::to_owned).collect())
                    .collect()
            })
            .unwrap_or_default()
    };
    let clients = tmux(&[
        "list-clients",
        "-F",
        "#{client_tty}\t#{client_activity}\t#{session_name}",
    ]);
    let panes = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{pane_current_command}\t#{pane_current_path}",
    ]);
    let here = Path::new(cwd).canonicalize().ok();
    let sessions = panes
        .iter()
        .filter(|p| {
            p.len() == 3
                && p[1] == "claude"
                && here.as_ref().is_some_and(|h| {
                    Path::new(&p[2])
                        .canonicalize()
                        .ok()
                        .is_some_and(|p| p.starts_with(h) || h.starts_with(p))
                })
        })
        .map(|p| &p[0])
        .collect::<Vec<_>>();
    let mut pool = clients
        .iter()
        .filter(|r| r.len() == 3 && sessions.contains(&&r[2]))
        .collect::<Vec<_>>();
    if pool.is_empty() {
        pool = clients.iter().filter(|r| r.len() == 3).collect();
    }
    let target = pool
        .into_iter()
        .max_by_key(|r| r[1].parse::<u64>().unwrap_or(0))
        .map(|r| r[0].clone())
        .or_else(|| {
            let p = invocation(
                vec!["ps".into(), "-axo".into(), "tty=,comm=".into()],
                None,
                ctx.cwd,
                Duration::from_secs(3),
                STATE_MAX,
                ctx,
            )
            .ok()?;
            String::from_utf8_lossy(&p.stdout).lines().find_map(|l| {
                let mut parts = l.split_whitespace();
                let tty = parts.next()?;
                let command = parts.next()?;
                (Path::new(command)
                    .file_name()
                    .is_some_and(|n| n == "claude")
                    && !["?", "??"].contains(&tty))
                .then(|| format!("/dev/{tty}"))
            })
        });
    if let Some(path) = target
        && path.starts_with("/dev/")
        && let Ok(mut f) = fs::OpenOptions::new()
            .write(true)
            .custom_flags(
                rustix::fs::OFlags::NOFOLLOW.bits() as i32
                    | rustix::fs::OFlags::NONBLOCK.bits() as i32,
            )
            .open(path)
        && f.metadata().is_ok_and(|m| m.file_type().is_char_device())
    {
        let _ = f.write_all(&data);
    }
    empty()
}
