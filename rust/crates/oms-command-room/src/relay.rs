//! Controller-owned, participant-pinned bounded private file queue.
use crate::storage::Directory;
use crate::value::{Value, encode, obj, parse, strings};
use crate::{Error, Result, Room, Send, checkpoint, identifier, unique};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const RELAY_MAX_REQUEST: usize = 8192;
pub const RELAY_MAX_RATE: usize = 24;
pub const RELAY_RATE_WINDOW: u64 = 60;
pub const RELAY_MAX_PENDING: usize = 32;
pub const RELAY_MAX_SCAN_ENTRIES: usize = 128;

pub struct Relay<'a, 'b> {
    core: &'a Room<'b>,
    directory: Directory,
    room: String,
    participant: String,
    parent: String,
    requests: VecDeque<Instant>,
    closed: Arc<AtomicBool>,
}
impl<'a, 'b> Relay<'a, 'b> {
    pub fn open(
        core: &'a Room<'b>,
        queue: &Path,
        room: &str,
        participant: &str,
        worktree: &Path,
    ) -> Result<Self> {
        identifier(room, "room")?;
        identifier(participant, "participant")?;
        let s = &core.session;
        if s.room.as_deref() != Some(room)
            || s.participant.as_deref() != Some(participant)
            || s.admitted_participant.as_deref() != Some(participant)
            || !s.relay_allowed
            || !s.child
            || !core.bound(room)?
        {
            return Err(Error::new(
                "room relay must be started by the bound controller",
            ));
        }
        let state = crate::project(&core.records(room)?)?;
        let p = crate::model::member(&state, participant)?;
        if p.text("role") == "main" || p.text("parent").is_empty() {
            return Err(Error::new(
                "room relay requires an admitted child participant",
            ));
        }
        let worktree = std::fs::canonicalize(worktree)
            .map_err(|_| Error::new("relay worktree unavailable"))?;
        let real =
            std::fs::canonicalize(queue).map_err(|_| Error::new("relay queue unavailable"))?;
        if real == worktree
            || !real.starts_with(&worktree)
            || std::fs::symlink_metadata(queue)
                .map_err(|_| Error::new("relay queue unavailable"))?
                .file_type()
                .is_symlink()
        {
            return Err(Error::new(
                "room relay queue must be a real directory inside the worker worktree",
            ));
        }
        let directory = Directory::open(&real)?;
        directory.private()?;
        Ok(Self {
            core,
            directory,
            room: room.to_owned(),
            participant: participant.to_owned(),
            parent: p.text("parent").to_owned(),
            requests: VecDeque::new(),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
    }
    pub fn shutdown_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.closed)
    }
    pub fn serve_forever(&mut self, poll: Duration) -> Result<()> {
        if poll.is_zero() || poll > Duration::from_secs(1) {
            return Err(Error::new(
                "relay poll interval must be positive and at most one second",
            ));
        }
        let owner = rustix::process::getppid();
        let (_ready, generation) = self.claim_endpoint()?;
        let result = (|| {
            while !self.closed.load(Ordering::Acquire)
                && !self.core.stop.load(Ordering::Acquire)
                && rustix::process::getppid() == owner
            {
                let (_, named) = self.directory.read(OsStr::new(".ready"), 0)?;
                if !crate::storage::stable(&generation, &named) {
                    return Err(Error::new("room relay endpoint ownership changed"));
                }
                self.handle_pending()?;
                let end = Instant::now() + poll;
                while Instant::now() < end
                    && !self.closed.load(Ordering::Acquire)
                    && !self.core.stop.load(Ordering::Acquire)
                {
                    std::thread::sleep(
                        Duration::from_millis(25)
                            .min(end.saturating_duration_since(Instant::now())),
                    );
                }
            }
            Ok(())
        })();
        // Never read .stop. Only this server's verified ready marker is cleaned.
        let _ = self
            .directory
            .unlink(OsStr::new(".ready"), Some(&generation));
        result
    }
    fn claim_endpoint(&self) -> Result<(OwnedFd, rustix::fs::Stat)> {
        use rustix::fs::{self, AtFlags, Mode, OFlags};
        let fd = fs::openat(
            &self.directory.fd,
            ".ready",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| Error::new("room relay endpoint unavailable"))?;
        let generation =
            fs::fstat(&fd).map_err(|_| Error::new("room relay endpoint unavailable"))?;
        let (_, named) = self.directory.read(OsStr::new(".ready"), 0)?;
        if !crate::storage::stable(&generation, &named) {
            return Err(Error::new("room relay endpoint changed"));
        }
        fs::flock(&fd, fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| Error::new("room relay endpoint is already owned or unavailable"))?;
        let named = fs::statat(&self.directory.fd, ".ready", AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| Error::new("room relay endpoint changed"))?;
        if !crate::storage::stable(&generation, &named) {
            return Err(Error::new("room relay endpoint changed"));
        }
        fs::fsync(&fd).map_err(|_| Error::new("room relay endpoint durability unavailable"))?;
        crate::storage::sync_dir(&self.directory.fd)?;
        Ok((fd, generation))
    }
    pub fn handle_request_data(&mut self, data: &[u8]) -> Result<Value> {
        if data.len() > RELAY_MAX_REQUEST {
            return Err(Error::new("room relay request is oversized"));
        }
        let request = parse(data, RELAY_MAX_REQUEST)?;
        if request.as_object().is_none() || request.text("room") != self.room {
            return Err(Error::new("room relay request names another room"));
        }
        if request.text("participant") != self.participant {
            return Err(Error::new("room relay request names another participant"));
        }
        let now = Instant::now();
        while self
            .requests
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(RELAY_RATE_WINDOW))
        {
            self.requests.pop_front();
        }
        if self.requests.len() >= RELAY_MAX_RATE {
            return Err(Error::new("room relay rate limit reached"));
        }
        self.requests.push_back(now);
        match request.text("action") {
            "send" => {
                let allowed = [
                    "action",
                    "room",
                    "participant",
                    "to",
                    "kind",
                    "text",
                    "reply_to",
                    "message_id",
                ];
                if request.as_object().is_none_or(|v| {
                    v.iter()
                        .any(|(k, _)| !allowed.iter().any(|a| k == a.as_bytes()))
                }) {
                    return Err(Error::new("unknown room relay request fields"));
                }
                if !request["to"].is_null() && request.text("to") != self.parent {
                    return Err(Error::new("a worker can only message its admitted parent"));
                }
                if !crate::model::KINDS.contains(&request.text("kind")) {
                    return Err(Error::new("invalid room message kind"));
                }
                let text = request["text"]
                    .as_str()
                    .ok_or_else(|| Error::new("message needs 1..4000 UTF-8 bytes"))?;
                let result = self.core.send(
                    &self.room,
                    Send {
                        sender: &self.participant,
                        recipient: &self.parent,
                        text,
                        kind: request.text("kind"),
                        reply_to: optional_string(&request, "reply_to")?,
                        message_id: optional_string(&request, "message_id")?,
                        linked: true,
                        quiet: false,
                    },
                )?;
                Ok(obj([
                    ("status", "sent".into()),
                    ("id", result["id"].clone()),
                ]))
            }
            "ack" => {
                let allowed = ["action", "room", "participant", "messages"];
                if request.as_object().is_none_or(|v| {
                    v.len() != allowed.len()
                        || v.iter()
                            .any(|(k, _)| !allowed.iter().any(|a| k == a.as_bytes()))
                }) {
                    return Err(Error::new("invalid room ack request fields"));
                }
                let messages = strings(&request["messages"])?;
                if messages.is_empty() || messages.len() > 200 {
                    return Err(Error::new("ack requires 1..200 exact message ids"));
                }
                for m in &messages {
                    identifier(m, "message")?;
                }
                self.core
                    .acknowledge(&self.room, &self.participant, &messages)?;
                Ok(obj([
                    ("status", "consumed".into()),
                    ("approval", false.into()),
                ]))
            }
            _ => Err(Error::new("room relay accepts send and ack only")),
        }
    }
    pub fn handle_pending(&mut self) -> Result<()> {
        self.directory.private()?;
        let (names, _) = self.directory.names(RELAY_MAX_SCAN_ENTRIES)?;
        let names = names
            .into_iter()
            .filter(|name| request_ticket(name).is_some())
            .take(RELAY_MAX_PENDING)
            .collect::<Vec<_>>();
        for name in names {
            if self.closed.load(Ordering::Acquire) || self.core.stop.load(Ordering::Acquire) {
                break;
            }
            let ticket = request_ticket(&name)
                .ok_or_else(|| Error::new("invalid relay ticket"))?
                .to_owned();
            let mut generation = None;
            let result = (|| {
                let (bytes, stat) = self.directory.read(&name, RELAY_MAX_REQUEST)?;
                generation = Some(stat);
                let mut request = parse(&bytes, RELAY_MAX_REQUEST)?;
                if request.text("action") == "send" && request["message_id"].is_null() {
                    // A kill after append but before queue cleanup must not give
                    // this same ticket another durable message on restart.
                    request.set("message_id", format!("msg-{ticket}"));
                }
                self.handle_request_data(&encode(&request, false, true)?)
            })();
            let response = match result {
                Ok(value) => obj([("ok", true.into()), ("result", value)]),
                Err(e) => obj([
                    ("ok", false.into()),
                    (
                        "error",
                        e.message.chars().take(240).collect::<String>().into(),
                    ),
                ]),
            };
            // Unsafe queue entries are unlinked by name, descriptor-relatively;
            // links/FIFOs are never followed and directories are never removed.
            let _ = self.directory.unlink(&name, generation.as_ref());
            let mut body = encode(&response, false, false)?;
            body.push(b'\n');
            if body.len() > RELAY_MAX_REQUEST {
                return Err(Error::new("room relay response exceeds size cap"));
            }
            let temp = OsString::from(format!(".tmp-{ticket}.json"));
            let response = OsString::from(format!("res-{ticket}.json"));
            let _ = self.directory.publish_queue(&temp, &response, &body);
        }
        Ok(())
    }
}
fn optional_string<'a>(request: &'a Value, k: &str) -> Result<Option<&'a str>> {
    if request[k].is_null() {
        Ok(None)
    } else {
        request[k]
            .as_str()
            .map(Some)
            .ok_or_else(|| Error::new("invalid room relay request field"))
    }
}
fn request_ticket(name: &OsStr) -> Option<&str> {
    let n = name.to_str()?;
    let ticket = n.strip_prefix("req-")?.strip_suffix(".json")?;
    crate::model::hex(ticket, 32).then_some(ticket)
}
/// Client cleanup is limited to its own request/response generation. A timeout
/// is uncertain: the server may already have appended the durable message.
/// An explicit message_id lets a caller reconcile rather than duplicate it.
pub fn relay_request(queue: &Path, request: &Value, interrupted: &AtomicBool) -> Result<Value> {
    checkpoint(interrupted)?;
    let mut bytes = encode(request, false, true)?;
    bytes.push(b'\n');
    if bytes.len() > RELAY_MAX_REQUEST {
        return Err(Error::new("room relay request is oversized"));
    }
    let directory = Directory::open(queue)?;
    directory.private()?;
    // Lock the existing directory inode, adding no queue metadata sidecar.
    // This serializes the count-and-publish step of cooperating clients.
    let admission = rustix::fs::openat(
        &directory.fd,
        ".",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| Error::new("room relay admission unavailable"))?;
    rustix::fs::flock(
        &admission,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(|_| Error::new("room relay admission is busy"))?;
    let (names, truncated) = directory.names(RELAY_MAX_SCAN_ENTRIES)?;
    if truncated
        || names.iter().filter(|n| request_ticket(n).is_some()).count() >= RELAY_MAX_PENDING
    {
        return Err(Error::new("room relay pending limit reached"));
    }
    let ticket = unique()?;
    let request_name = OsString::from(format!("req-{ticket}.json"));
    let temporary = OsString::from(format!(".tmp-req-{ticket}"));
    let response_name = OsString::from(format!("res-{ticket}.json"));
    directory.publish_queue(&temporary, &request_name, &bytes)?;
    drop(admission);
    let request_stat = rustix::fs::statat(
        &directory.fd,
        &request_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .ok();
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = (|| {
        loop {
            checkpoint(interrupted)?;
            if directory.exists(&response_name)? {
                let (body, stat) = directory.read(&response_name, RELAY_MAX_REQUEST)?;
                directory.unlink(&response_name, Some(&stat))?;
                if !body.ends_with(b"\n") {
                    return Err(Error::new("room relay returned an invalid response"));
                }
                let decoded = parse(&body, RELAY_MAX_REQUEST)?;
                if decoded["ok"].as_bool() != Some(true) {
                    return Err(Error::new(
                        decoded["error"]
                            .as_str()
                            .unwrap_or("room relay refused the request"),
                    ));
                }
                return decoded
                    .get("result")
                    .cloned()
                    .ok_or_else(|| Error::new("room relay returned an invalid response"));
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    "room relay controller did not answer within 10 seconds",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    if let Some(stat) = request_stat {
        let _ = directory.unlink(&request_name, Some(&stat));
    }
    result
}
