use crate::broker::Backend;
use crate::common::*;
use crate::messages;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

struct State {
    backend: Arc<dyn Backend>,
    model: String,
    effort: Option<String>,
    supported: Vec<String>,
    capability: String,
    host: String,
    slots: (Mutex<usize>, Condvar),
    connections: Mutex<Vec<(u64, TcpStream)>>,
    stop: AtomicBool,
}
pub(crate) struct Gateway {
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}
struct Slot {
    state: Arc<State>,
    id: u64,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut n) = self.state.slots.0.lock() {
            *n = n.saturating_sub(1);
            self.state.slots.1.notify_one();
        }
        if let Ok(mut sockets) = self.state.connections.lock() {
            sockets.retain(|(id, _)| *id != self.id);
        }
    }
}
impl Gateway {
    pub fn start(
        backend: Arc<dyn Backend>,
        model: String,
        effort: Option<String>,
        supported: Vec<String>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let host = listener.local_addr()?.to_string();
        let state = Arc::new(State {
            backend,
            model,
            effort,
            supported,
            capability: crate::https::random_hex(32)?,
            host,
            slots: (Mutex::new(0), Condvar::new()),
            connections: Mutex::new(vec![]),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&state);
        let thread = std::thread::spawn(move || {
            let mut id = 0u64;
            let mut workers = vec![];
            while !worker.stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Ok(mut slots) = worker.slots.0.lock() else {
                            break;
                        };
                        if *slots >= 8 {
                            drop(stream);
                            continue;
                        }
                        *slots += 1;
                        drop(slots);
                        id += 1;
                        let guard = Slot {
                            state: Arc::clone(&worker),
                            id,
                        };
                        if let Ok(copy) = stream.try_clone()
                            && let Ok(mut sockets) = worker.connections.lock()
                        {
                            sockets.push((id, copy));
                        }
                        workers.push(std::thread::spawn(move || {
                            let _ = handle(stream, &guard.state);
                            drop(guard);
                        }));
                        let mut pending = vec![];
                        for join in workers {
                            if join.is_finished() {
                                let _ = join.join();
                            } else {
                                pending.push(join);
                            }
                        }
                        workers = pending;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
            for join in workers {
                let _ = join.join();
            }
        });
        Ok(Self {
            state,
            thread: Some(thread),
        })
    }
    pub fn url(&self) -> String {
        format!("http://{}", self.state.host)
    }
    pub fn capability(&self) -> &str {
        &self.state.capability
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        if let Ok(sockets) = self.state.connections.lock() {
            for (_, s) in sockets.iter() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut delta = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        delta |=
            usize::from(a.get(index).copied().unwrap_or(0) ^ b.get(index).copied().unwrap_or(0));
    }
    delta == 0
}
fn reply(stream: &mut TcpStream, status: u16, row: Value) -> Result<()> {
    let data = row.to_string();
    write!(
        stream,
        "HTTP/1.0 {status} result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-should-retry: {}\r\n\r\n{data}",
        data.len(),
        if status == 502 || status == 429 {
            "true"
        } else {
            "false"
        }
    )?;
    stream.flush()?;
    Ok(())
}
fn sse(stream: &mut TcpStream, name: &str, mut row: Value) -> Result<()> {
    if !row.is_object() {
        return Err(error("invalid stream event"));
    }
    if row.get("type").is_none() {
        row["type"] = json!(name);
    }
    if name.chars().any(char::is_control) {
        return Err(error("invalid stream event type"));
    }
    write!(stream, "event: {name}\ndata: {row}\n\n")?;
    stream.flush()?;
    Ok(())
}
fn handle(mut stream: TcpStream, state: &State) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.by_ref().take(8193).read_line(&mut first)?;
    if first.len() > 8192 {
        return reply(
            &mut stream,
            413,
            json!({"error":{"message":"Request headers exceed limit"}}),
        );
    }
    let words: Vec<_> = first.split_whitespace().collect();
    if words.len() != 3 {
        return reply(
            &mut stream,
            400,
            json!({"error":{"message":"Invalid request"}}),
        );
    }
    let method = words[0];
    let path = words[1].split('?').next().unwrap_or("");
    let mut headers = BTreeMap::new();
    let mut total = first.len();
    loop {
        let mut line = String::new();
        let count = reader.by_ref().take(8193).read_line(&mut line)?;
        total += count;
        if count == 0 || total > 65536 || line.len() > 8192 {
            return reply(
                &mut stream,
                413,
                json!({"error":{"message":"Request headers exceed limit"}}),
            );
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.trim_end_matches(['\r', '\n']).split_once(':') else {
            return reply(
                &mut stream,
                400,
                json!({"error":{"message":"Invalid headers"}}),
            );
        };
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || headers
                .insert(name.to_ascii_lowercase(), value.trim().to_owned())
                .is_some()
        {
            return reply(
                &mut stream,
                400,
                json!({"error":{"message":"Invalid duplicate headers"}}),
            );
        }
    }
    let get = |name: &str| headers.get(name).map(String::as_str).unwrap_or("");
    let valid = get("host") == state.host
        && !headers.contains_key("origin")
        && (equal(
            get("authorization").as_bytes(),
            format!("Bearer {}", state.capability).as_bytes(),
        ) || equal(get("x-api-key").as_bytes(), state.capability.as_bytes()));
    if !valid {
        return reply(
            &mut stream,
            403,
            json!({"error":{"type":"authentication_error","message":"Invalid local bridge authentication"}}),
        );
    }
    if method == "HEAD" {
        write!(stream, "HTTP/1.0 200 OK\r\n\r\n")?;
        return Ok(());
    }
    if method != "POST" || !["/v1/messages", "/v1/responses"].contains(&path) {
        return reply(
            &mut stream,
            404,
            json!({"error":{"type":"not_found_error","message":"Unsupported bridge endpoint"}}),
        );
    }
    let length = if headers.contains_key("transfer-encoding") {
        None
    } else {
        number(get("content-length"), 1, LIMIT, "Content-Length").ok()
    };
    let Some(length) = length else {
        return reply(
            &mut stream,
            413,
            json!({"error":{"message":"Request body must have a bounded Content-Length"}}),
        );
    };
    let mut bytes = vec![0; length];
    if reader.read_exact(&mut bytes).is_err() {
        return reply(
            &mut stream,
            400,
            json!({"error":{"message":"Incomplete request body"}}),
        );
    }
    let body = match json(&bytes) {
        Ok(v) if v.is_object() && v.get("stream").is_none_or(Value::is_boolean) => v,
        _ => {
            return reply(
                &mut stream,
                400,
                json!({"error":{"message":"Invalid bridge request"}}),
            );
        }
    };
    let translated = if path == "/v1/messages" {
        messages::from_messages(&body)
    } else {
        Ok(body.clone())
    };
    let outbound = match translated.and_then(|v| {
        messages::subscription(&v, &state.model, state.effort.as_deref(), &state.supported)
    }) {
        Ok(v) => v,
        Err(_) => {
            return reply(
                &mut stream,
                400,
                json!({"error":{"message":"Invalid or unsupported bridge request"}}),
            );
        }
    };
    let mut response = match state.backend.open(&outbound) {
        Ok(v) => v,
        Err(_) => {
            return reply(
                &mut stream,
                502,
                json!({"error":{"type":"server_error","message":"Codex upstream request failed"}}),
            );
        }
    };
    if response.status != 200 {
        let mut raw = vec![];
        response.stream.take(65536).read_to_end(&mut raw)?;
        let row = json(&raw).unwrap_or(Value::Null);
        let (status, kind, message) = classify(&row, Some(response.status));
        return reply(
            &mut stream,
            status,
            json!({"error":{"type":kind,"code":kind,"message":message}}),
        );
    }
    let streaming = body["stream"] == true;
    let mut converter = messages::MessageStream::new(&state.model);
    if streaming {
        write!(
            stream,
            "HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\r\n"
        )?;
        stream.flush()?;
    }
    let result = (|| {
        let mut payload = String::new();
        let mut size = 0usize;
        loop {
            if state.stop.load(Ordering::Relaxed) {
                return Err(error("bridge stopped"));
            }
            let mut raw = vec![];
            let count = response
                .stream
                .by_ref()
                .take((LIMIT + 1) as u64)
                .read_until(b'\n', &mut raw)?;
            size = size.saturating_add(count);
            if count == 0 {
                return Err(error("Codex response ended before completion"));
            }
            if size > LIMIT {
                return Err(error("Codex stream event exceeded the size limit"));
            }
            let line = std::str::from_utf8(&raw)
                .map_err(|_| error("invalid SSE text"))?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if payload.is_empty() {
                    if streaming {
                        sse(&mut stream, "ping", json!({}))?;
                    }
                    size = 0;
                    continue;
                }
                let event = json(payload.trim_end_matches('\n').as_bytes())?;
                payload.clear();
                size = 0;
                let kind = event["type"]
                    .as_str()
                    .ok_or_else(|| error("invalid stream event"))?;
                if matches!(kind, "error" | "response.failed") {
                    return Err(error("Codex inference failed"));
                }
                let terminal = matches!(kind, "response.completed" | "response.incomplete");
                if terminal
                    && event["response"]["status"]
                        != if kind == "response.completed" {
                            "completed"
                        } else {
                            "incomplete"
                        }
                {
                    return Err(error("inconsistent completion event"));
                }
                if streaming {
                    let rows = if path == "/v1/messages" {
                        converter.feed(&event)?
                    } else {
                        vec![(kind.to_owned(), event.clone())]
                    };
                    for (name, row) in rows {
                        sse(&mut stream, &name, row)?;
                    }
                }
                if terminal {
                    if !streaming {
                        let result = if path == "/v1/messages" {
                            messages::message(
                                &event["response"],
                                &state.model,
                                &messages::stops(&body)?,
                            )?
                        } else {
                            event["response"].clone()
                        };
                        reply(&mut stream, 200, result)?;
                    }
                    return Ok(());
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                if value.trim() == "[DONE]" {
                    continue;
                }
                payload.push_str(value.trim_start_matches(' '));
                payload.push('\n');
            } else if !["event:", "id:", "retry:", ":"]
                .iter()
                .any(|s| line.starts_with(s))
            {
                return Err(error("unsupported response format"));
            }
        }
    })();
    if result.is_err() {
        if streaming {
            sse(
                &mut stream,
                "error",
                json!({"error":{"type":"api_error","message":"Codex response stream failed"}}),
            )?;
        } else {
            reply(
                &mut stream,
                502,
                json!({"error":{"type":"api_error","message":"Codex response stream failed"}}),
            )?;
        }
    }
    Ok(())
}
fn classify(row: &Value, status: Option<u16>) -> (u16, &'static str, &'static str) {
    let row = row.get("response").unwrap_or(row);
    let row = row.get("error").unwrap_or(row);
    let values = [
        row["code"].as_str().unwrap_or(""),
        row["type"].as_str().unwrap_or(""),
    ];
    if values.iter().any(|v| {
        [
            "usage_limit_reached",
            "usage_limit_exceeded",
            "usage_not_included",
            "insufficient_quota",
            "quota_exceeded",
        ]
        .contains(v)
    }) {
        (429, "usage_limit_reached", "Codex usage limit reached")
    } else if values.contains(&"context_length_exceeded") {
        (
            400,
            "context_length_exceeded",
            "Codex maximum context length exceeded",
        )
    } else if matches!(status, Some(401 | 403)) {
        (
            status.unwrap_or(401),
            "authentication_error",
            "Codex subscription authentication failed",
        )
    } else if status.is_none() || status.is_some_and(|v| v == 429 || v >= 500) {
        (
            if status == Some(429) { 429 } else { 502 },
            "server_error",
            "Codex server error; temporarily unavailable",
        )
    } else {
        (
            status
                .filter(|s| [400, 404, 422].contains(s))
                .unwrap_or(502),
            "upstream_error",
            "Codex upstream request failed",
        )
    }
}
