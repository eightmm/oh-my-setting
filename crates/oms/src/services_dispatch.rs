//! Linux controller-only dispatch custody. The abstract address is a locator.
//! The socket is authenticated against the two live, kernel-observed processes.
use oms_command_peer::process::Invocation;
use oms_command_telemetry::store::usage::Identity;
use oms_panel_core::services::{DispatchLaunch, DispatchProcess, DispatchProof};
use oms_reference::process::{IdentitySource, NativeIdentity};
use oms_supervisor::process::{Completion, JobId, PhysicalCall, Reactor, Scope};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const FLAG: &str = "--panel-custody";
const MAX_FRAME: usize = 16384;
const HANDSHAKE: Duration = Duration::from_secs(2);

fn identity(pid: u32) -> Option<DispatchProcess> {
    let owner = NativeIdentity.owner_identity(pid).ok()?;
    Some(DispatchProcess {
        pid: owner.pid,
        birth: owner.birth,
    })
}

fn peer_matches(socket: &UnixStream, expected: &DispatchProcess) -> bool {
    rustix::net::sockopt::socket_peercred(socket)
        .ok()
        .is_some_and(|peer| {
            peer.uid == rustix::process::getuid()
                && u32::try_from(peer.pid.as_raw_pid()).ok() == Some(expected.pid)
                && identity(expected.pid).as_ref() == Some(expected)
        })
}

fn encode(value: Value) -> Option<Vec<u8>> {
    let bytes = serde_json::to_vec(&value).ok()?;
    (bytes.len() <= MAX_FRAME).then_some(bytes)
}

fn read_frame(mut socket: &UnixStream) -> Option<Value> {
    let deadline = Instant::now() + HANDSHAKE;
    let mut bytes = Vec::new();
    let mut buffer = [0; MAX_FRAME + 1];
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        socket.set_read_timeout(Some(remaining)).ok()?;
        let n = socket.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > MAX_FRAME {
            return None;
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    oms_panel_core::decode(&bytes, MAX_FRAME).ok()
}

fn address(locator: &str) -> Option<SocketAddr> {
    (locator.len() == 32
        && locator
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
    .then_some(())?;
    SocketAddr::from_abstract_name(format!("oms-panel-custody-{locator}")).ok()
}

pub(super) struct Controller {
    pub(super) locator: String,
    launch: DispatchLaunch,
    own: DispatchProcess,
    call: PhysicalCall,
    binary: (u64, u64),
    stage: Option<DispatchProcess>,
    listener: Option<UnixListener>,
    stream: Option<UnixStream>,
    deadline: Instant,
    frame: Option<Vec<u8>>,
    closed: bool,
}
impl Controller {
    pub(super) fn new(launch: &DispatchLaunch, binary: &Path) -> Option<Self> {
        let locator = oms_command_plan::storage::random_hex(16).ok()?;
        let meta = std::fs::metadata(binary).ok()?;
        let listener = UnixListener::bind_addr(&address(&locator)?).ok()?;
        listener.set_nonblocking(true).ok()?;
        Some(Self {
            locator,
            launch: launch.clone(),
            own: identity(std::process::id())?,
            call: PhysicalCall::new(&format!("dispatch:{}", launch.nonce)).ok()?,
            binary: (meta.dev(), meta.ino()),
            stage: None,
            listener: Some(listener),
            stream: None,
            deadline: Instant::now() + HANDSHAKE,
            frame: None,
            closed: false,
        })
    }

    pub(super) fn call(&self) -> PhysicalCall {
        self.call.clone()
    }

    fn accept(&mut self) {
        let Some(listener) = &self.listener else {
            return;
        };
        if Instant::now() >= self.deadline {
            self.listener = None;
            return;
        }
        let Some(stage) = self.stage.as_ref() else {
            return;
        };
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        if !peer_matches(&stream, stage) || executable_inode(stage.pid) != Some(self.binary) {
            return;
        }
        self.listener = None;
        let Ok(launch) = serde_json::to_value(&self.launch) else {
            return;
        };
        let Some(frame) = encode(json!({
            "launch":launch, "controller":self.own, "stage":stage
        })) else {
            return;
        };
        if stream.set_nonblocking(true).is_ok()
            && stream.write_all(&frame).is_ok()
            && stream.shutdown(Shutdown::Write).is_ok()
        {
            self.stream = Some(stream);
            self.frame = Some(Vec::new());
        }
    }

    fn poll(&mut self) {
        self.accept();
        let (Some(stream), Some(frame)) = (&mut self.stream, &mut self.frame) else {
            return;
        };
        let mut buffer = [0u8; MAX_FRAME + 1];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    self.stream = None;
                    self.closed = true;
                    return;
                }
                Ok(n) if frame.len() + n <= MAX_FRAME => frame.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                _ => {
                    self.stream = None;
                    self.frame = None;
                    return;
                }
            }
        }
    }

    pub(super) fn proof(&self, done: &Completion) -> Option<DispatchProof> {
        let stage = self.stage.as_ref()?;
        if !self.closed
            || done.pid != stage.pid
            || done.exit?.code != Some(0)
            || !done.owned_cleanup().is_some_and(|proof| {
                proof.pid() == stage.pid && proof.matches(&self.call, Scope::WholeTree)
            })
            || !normal_completion(done)
        {
            return None;
        }
        self.record(stage)
    }

    fn record(&self, stage: &DispatchProcess) -> Option<DispatchProof> {
        let record = oms_panel_core::decode(self.frame.as_deref()?, MAX_FRAME).ok()?;
        if record.as_object()?.len() != 1 {
            return None;
        }
        let proof: DispatchProof = serde_json::from_value(record.get("done")?.clone()).ok()?;
        (proof.launch == self.launch
            && proof.stage == *stage
            && proof.provider.pid != stage.pid
            && proof.provider.pid != self.own.pid)
            .then_some(proof)
    }
}

pub(super) struct Stage {
    stream: UnixStream,
    launch: DispatchLaunch,
    own: DispatchProcess,
    controller: DispatchProcess,
    calls: usize,
    completed: Option<DispatchProof>,
}
impl Stage {
    pub(super) fn connect(locator: &str, cwd: &Path) -> Option<Self> {
        // The stream belongs to this staged host and is close-on-exec. The
        // provider receives neither the socket nor its locator.
        let target = address(locator)?;
        let sock_addr =
            rustix::net::SocketAddrUnix::new_abstract_name(target.as_abstract_name()?).ok()?;
        let fd = rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::CLOEXEC | rustix::net::SocketFlags::NONBLOCK,
            None,
        )
        .ok()?;
        rustix::net::connect(&fd, &sock_addr).ok()?;
        let stream = UnixStream::from(fd);
        stream.set_nonblocking(false).ok()?;
        let parent = rustix::process::getppid()?;
        let controller = identity(u32::try_from(parent.as_raw_pid()).ok()?)?;
        if !peer_matches(&stream, &controller)
            || executable_inode(controller.pid)? != executable_inode(std::process::id())?
        {
            return None;
        }
        let offer = read_frame(&stream)?;
        if offer.as_object()?.len() != 3 {
            return None;
        }
        let launch: DispatchLaunch = serde_json::from_value(offer.get("launch")?.clone()).ok()?;
        let own = identity(std::process::id())?;
        if offer["controller"] != json!(controller)
            || offer["stage"] != json!(own)
            || !peer_matches(&stream, &controller)
            || launch.execution != std::fs::canonicalize(cwd).ok()?
            || launch.repo != std::fs::canonicalize(&launch.repo).ok()?
        {
            return None;
        }
        Some(Self {
            stream,
            launch,
            own,
            controller,
            calls: 0,
            completed: None,
        })
    }

    pub(super) fn ineligible_call(&mut self) {
        self.calls = self.calls.saturating_add(1);
    }

    pub(super) fn begin(
        &mut self,
        request: &Invocation,
        call: &Identity,
        direct: bool,
    ) -> Option<Provider> {
        self.ineligible_call();
        let att = call.attempt_id.as_ref()?;
        let expected = request.physical_call.as_ref()?;
        if self.calls != 1
            || !direct
            || call.provider != self.launch.provider
            || call.task_id.as_deref() != Some(self.launch.task.as_str())
            || call.call_id != expected.id()
        {
            return None;
        }
        Some(Provider {
            binary: executable_identity(request, &call.provider)?,
            call: expected.clone(),
            attempt: att.clone(),
            owner: None,
        })
    }

    pub(super) fn complete(&mut self, source: Provider, done: &Completion) {
        let Some(owner) = source.owner else {
            return;
        };
        if self.calls == 1
            && let (Some(exit), Some(proof)) = (done.exit, done.owned_cleanup())
            && done.pid == owner.pid
            && proof.pid() == owner.pid
            && proof.matches(&source.call, Scope::WholeTree)
            && normal_completion(done)
        {
            self.completed = Some(DispatchProof {
                launch: self.launch.clone(),
                stage: self.own.clone(),
                provider: owner,
                call: source.call.id().into(),
                attempt: source.attempt,
                exit: exit.shell_code(),
            });
        }
    }

    pub(super) fn finish(mut self, code: i32) {
        if code == 0
            && self.calls == 1
            && peer_matches(&self.stream, &self.controller)
            && let Some(proof) = self.completed
            && let Ok(proof) = serde_json::to_value(proof)
            && let Some(frame) = encode(json!({"done":proof}))
            && self.stream.set_nonblocking(true).is_ok()
        {
            let _ = self.stream.write_all(&frame);
            let _ = self.stream.shutdown(Shutdown::Write);
        }
    }
}

fn normal_completion(done: &Completion) -> bool {
    done.stop.is_none()
        && done.cleanup_complete()
        && done.output_complete()
        && !done.stdout.limited
        && done.stderr.as_ref().is_none_or(|stream| !stream.limited)
}

fn executable_inode(pid: u32) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
    Some((meta.dev(), meta.ino()))
}

fn executable_identity(request: &Invocation, provider: &str) -> Option<(u64, u64)> {
    let binary = match provider {
        "codex" | "claude" => provider,
        "antigravity" => "agy",
        _ => return None,
    };
    if request.argv.first()?.as_os_str() != OsStr::new(binary) {
        return None;
    }
    let key = OsStr::new("PATH");
    let path = request
        .environment
        .iter()
        .rev()
        .find_map(|(name, value)| (name == key).then(|| value.clone()))
        .or_else(|| {
            (!request.remove_environment.iter().any(|name| name == key))
                .then(|| std::env::var_os(key))
                .flatten()
        })?;
    std::env::split_paths(&path)
        .map(|dir: PathBuf| request.cwd.join(dir).join(binary))
        .find_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            (meta.is_file() && meta.mode() & 0o111 != 0).then(|| (meta.dev(), meta.ino()))
        })
}

pub(super) struct Provider {
    binary: (u64, u64),
    call: PhysicalCall,
    attempt: String,
    owner: Option<DispatchProcess>,
}

pub(super) enum Observer<'a> {
    Stage(&'a mut Controller),
    Provider(&'a mut Provider),
}
impl Observer<'_> {
    pub(super) fn started(&mut self, reactor: &Reactor, job: JobId) {
        let Some(snapshot) = reactor.snapshot(job).ok() else {
            return;
        };
        let Some(owned) = identity(snapshot.pid) else {
            return;
        };
        match self {
            Self::Stage(controller) => controller.stage = Some(owned),
            Self::Provider(provider) => {
                let Some(pid) = rustix::process::Pid::from_raw(snapshot.pid as i32) else {
                    return;
                };
                if rustix::process::getpgid(Some(pid)).ok() == Some(pid)
                    && executable_inode(owned.pid) == Some(provider.binary)
                    && identity(snapshot.pid).as_ref() == Some(&owned)
                {
                    provider.owner = Some(owned);
                }
            }
        }
    }

    pub(super) fn poll(&mut self) {
        if let Self::Stage(controller) = self {
            controller.poll();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oms_supervisor::process::{Progress, Request, StopReason};

    fn controller() -> Controller {
        Controller::new(
            &DispatchLaunch {
                nonce: "1".repeat(32),
                repo: "/fixture/repo".into(),
                execution: "/fixture/repo".into(),
                room: "room-fixture".into(),
                main: "main-fixture".into(),
                main_attempt: "attempt-main".into(),
                main_created: "fixture-generation".into(),
                main_provider: "codex".into(),
                member: "call-fixture".into(),
                task: "task-fixture".into(),
                provider: "claude".into(),
                membership_sha256: "2".repeat(64),
                reservation_sha256: "3".repeat(64),
                reservation_generation: [1, 2, 3, 4, 5],
            },
            &std::env::current_exe().unwrap(),
        )
        .unwrap()
    }

    fn record(controller: &Controller, stage: DispatchProcess) -> DispatchProof {
        DispatchProof {
            launch: controller.launch.clone(),
            stage,
            provider: DispatchProcess {
                pid: u32::MAX,
                birth: "fixture-native-generation".into(),
            },
            call: "fixture-native".into(),
            attempt: "attempt-fixture".into(),
            exit: 0,
        }
    }

    #[test]
    fn protected_dispatch_refuses_foreign_credentials_replayed_and_partial_frames() {
        let (socket, _) = UnixStream::pair().unwrap();
        let mut controller = controller();
        let stage = controller.own.clone();
        assert!(peer_matches(&socket, &stage));
        let foreign = DispatchProcess {
            pid: stage.pid + 1,
            birth: stage.birth.clone(),
        };
        assert!(!peer_matches(&socket, &foreign));
        let reused = DispatchProcess {
            pid: stage.pid,
            birth: "stale-generation".into(),
        };
        assert!(!peer_matches(&socket, &reused));
        assert!(controller.record(&stage).is_none());

        let mut proof = record(&controller, stage.clone());
        controller.frame = encode(json!({"done":proof}));
        assert!(controller.record(&stage).is_some());
        assert!(controller.record(&reused).is_none());
        let bytes = controller.frame.take().unwrap();
        controller.frame = Some(bytes[..bytes.len() - 1].to_vec());
        assert!(controller.record(&stage).is_none());
        controller.frame = Some(vec![0; MAX_FRAME + 1]);
        assert!(controller.record(&stage).is_none());
        controller.frame = Some([bytes.as_slice(), bytes.as_slice()].concat());
        assert!(controller.record(&stage).is_none());
        proof.launch.nonce = "4".repeat(32);
        controller.frame = encode(json!({"done":proof}));
        assert!(controller.record(&stage).is_none());
    }

    #[test]
    fn protected_dispatch_refuses_group_cleanup_and_interruption() {
        for stop in [None, Some(StopReason::Cancelled)] {
            let mut controller = controller();
            let mut reactor = Reactor::new();
            let mut request =
                Request::new(["/bin/sh", "-c", "read -r ready"].map(Into::into).to_vec());
            request.stdin = Some(b"ready\n".to_vec());
            request.scope = Scope::ProcessGroup;
            request.physical_call = Some(controller.call());
            let job = reactor.start(request).unwrap();
            let stage = identity(reactor.snapshot(job).unwrap().pid).unwrap();
            controller.frame = encode(json!({"done":record(&controller, stage.clone())}));
            controller.stage = Some(stage);
            controller.closed = true;
            if let Some(reason) = stop {
                reactor.stop(job, reason).unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let done = loop {
                match reactor.poll(job, Duration::from_millis(50)).unwrap() {
                    Progress::Pending(_) => assert!(Instant::now() < deadline),
                    Progress::Complete(done) => break done,
                }
            };
            assert_eq!(done.stop, stop);
            if stop.is_none() {
                assert!(
                    done.owned_cleanup()
                        .unwrap()
                        .matches(&controller.call, Scope::ProcessGroup)
                );
            } else {
                assert!(!normal_completion(&done));
            }
            assert!(controller.proof(&done).is_none());
        }
    }
}
