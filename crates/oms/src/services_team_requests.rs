//! Native-owned read-only request channel. Public locators and capability declarations
//! select data; only this actual New/child/pipe issuer creates a session.
use super::{CollabServices, Job, prelaunch_error, supervise};
use oms_command_peer::process::{Completed, Invocation, TeamLaunchBinding, TeamRequestAction};
use oms_command_peer::{Error, Result};
use oms_command_plan::{
    Action, Context, Plan, Request,
    json::{self, Value, number, object, string},
};
use oms_reference::process::{IdentitySource, NativeIdentity, Owner};
use oms_supervisor::process::{DuplexOptions, JobId, OutputEvent, OutputStream, Reactor};
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn err(message: impl Into<String>) -> Error {
    Error::new(75, message)
}
fn plan_error(e: oms_command_plan::Error) -> Error {
    Error::new(e.code, e.message)
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}
fn now() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(i128::MAX, |d| d.as_secs() as i128)
}
pub(super) fn options() -> DuplexOptions {
    DuplexOptions {
        max_frame_bytes: 8192,
        max_input_bytes: 32768,
        max_input_frames: 4,
        max_output_bytes: 65536,
        max_output_events: 8,
        max_read_bytes_per_turn: 4096,
        max_write_bytes_per_turn: 4096,
    }
}

/// Code-owned issuance boundaries only; provider frames and storage errors may
/// contain the private session, so they must not supply these outward labels.
#[derive(Clone, Copy)]
pub(super) enum IssuanceStage {
    Unavailable,
    ProcessSnapshot,
    NativeIdentity,
    DuplexConnection,
    SessionRandomness,
    ControllerCheck,
    SessionRecord,
    PrivateStore,
    IssuanceLock,
    PrivatePublication,
    PrivateBinding,
    AdmissionRecheck,
    HelloDelivery,
}
impl IssuanceStage {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::ProcessSnapshot => "process-snapshot",
            Self::NativeIdentity => "native-identity",
            Self::DuplexConnection => "duplex-connection",
            Self::SessionRandomness => "session-randomness",
            Self::ControllerCheck => "controller-check",
            Self::SessionRecord => "session-record",
            Self::PrivateStore => "private-store",
            Self::IssuanceLock => "issuance-lock",
            Self::PrivatePublication => "private-publication",
            Self::PrivateBinding => "private-binding",
            Self::AdmissionRecheck => "admission-recheck",
            Self::HelloDelivery => "hello-delivery",
        }
    }
}

struct PrivateBinding {
    path: PathBuf,
    raw: Vec<u8>,
    descriptor: std::fs::File,
    generation: rustix::fs::Stat,
}
fn same_file(a: &rustix::fs::Stat, b: &rustix::fs::Stat) -> bool {
    a.st_dev == b.st_dev
        && a.st_ino == b.st_ino
        && a.st_size == b.st_size
        && a.st_uid == b.st_uid
        && a.st_gid == b.st_gid
        && a.st_mode == b.st_mode
        && a.st_nlink == b.st_nlink
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
impl PrivateBinding {
    fn open(path: PathBuf, raw: Vec<u8>) -> Result<Self> {
        use rustix::fs::{Mode, OFlags};
        let descriptor: std::fs::File = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| err("private Team binding unavailable"))?
        .into();
        let generation =
            rustix::fs::fstat(&descriptor).map_err(|_| err("private Team binding unavailable"))?;
        if rustix::fs::FileType::from_raw_mode(generation.st_mode)
            != rustix::fs::FileType::RegularFile
            || generation.st_uid != rustix::process::geteuid().as_raw()
            || generation.st_mode & 0o077 != 0
            || generation.st_nlink != 1
        {
            return Err(err("unsafe private Team operation binding"));
        }
        let binding = Self {
            path,
            raw,
            descriptor,
            generation,
        };
        binding.check()?;
        Ok(binding)
    }
    fn check(&self) -> Result<()> {
        let current = rustix::fs::fstat(&self.descriptor)
            .map_err(|_| err("private Team binding unavailable"))?;
        let named = rustix::fs::statat(
            rustix::fs::CWD,
            &self.path,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| err("private Team binding unavailable"))?;
        if !same_file(&self.generation, &current) || !same_file(&current, &named) {
            return Err(err("private Team binding generation changed"));
        }
        let store =
            oms_panel_core::Store::open(self.path.parent().unwrap()).map_err(|e| err(e.message))?;
        if store
            .read(Path::new("session.json"), 8192, false)
            .map_err(|e| err(e.message))?
            != self.raw
        {
            return Err(err("private Team binding changed"));
        }
        Ok(())
    }
}

pub(super) struct Channel<'a, 's> {
    service: &'a CollabServices<'s>,
    context: Context,
    admission: oms_command_plan::team::AdmissionRequest,
    binding: Value,
    binding_sha: String,
    attempt: String,
    created: String,
    participant: String,
    enrollment: i128,
    call: String,
    operation: String,
    deadline: Instant,
    expires_at: i128,
    stop: &'a AtomicBool,
    controller: oms_reference::controller::Controller,
    owner: Option<Owner>,
    session: String,
    nonce: String,
    generation: u64,
    event_sequence: u64,
    sequence: i128,
    hello: bool,
    buffer: Vec<u8>,
    responses: Vec<(String, i128, String)>,
    consumed: usize,
    actions: Vec<TeamRequestAction>,
    private: Option<PrivateBinding>,
    issuance_stage: IssuanceStage,
}
impl<'a, 's> Channel<'a, 's> {
    fn prepare(
        service: &'a CollabServices<'s>,
        launch: &TeamLaunchBinding<'_>,
        stop: &'a AtomicBool,
        deadline: Instant,
    ) -> Result<Self> {
        launch.check(service, stop)?;
        let selected = launch.attempt();
        let team = selected
            .team
            .ok_or_else(|| err("Team channel needs a canonical member"))?;
        if selected.write {
            return Err(err("Team channel requires read access"));
        }
        let cwd = std::env::current_dir()?;
        let context = Context::from_environment_before(
            &Request::new(Action::Status),
            selected.contract_repo,
            &cwd,
            Some(selected.state_root),
            stop,
            deadline,
        )
        .map_err(plan_error)?;
        let admission = oms_command_plan::team::AdmissionRequest {
            team_id: team.team_id.clone(),
            spec_sha256: team.spec_sha256.clone(),
            member_id: team.member_id.clone(),
            member_generation: team.member_generation,
            provider: selected.provider.into(),
            harness: selected.provider.into(),
            host: "local".into(),
            transport: "direct".into(),
            account: selected.account.map(str::to_owned),
            selected_model: selected.selected_model.map(str::to_owned),
            selected_effort: selected.selected_effort.map(str::to_owned),
            write: false,
        };
        let plan = Plan::open(context.clone(), &service.control, stop).map_err(plan_error)?;
        let guard = plan
            .team_admission_before(admission.clone(), deadline)
            .map_err(plan_error)?;
        if guard.binding().get("member").text("role") != "lead" {
            return Err(err("Team request sender is not the canonical lead"));
        }
        let binding = guard.binding().clone();
        let expires_at = (guard.expires_at_unix().map_err(plan_error)?.unwrap_or(0) as i128)
            .min(now() + deadline.saturating_duration_since(Instant::now()).as_secs() as i128);
        let identity = launch.identity();
        let attempt = identity
            .attempt_id
            .clone()
            .ok_or_else(|| err("Team lead has no actual lifecycle attempt"))?;
        let (events, projected) =
            oms_command_operations::load_projection(selected.state_root).map_err(err)?;
        let found = projected
            .iter()
            .find(|v| v.get("attempt_id").text() == Some(&attempt))
            .ok_or_else(|| err("Team lead lifecycle unavailable"))?;
        if found.get("tool").text() != Some("peer-ask")
            || found.get("state").text() != Some("working")
            || found.get("parent_attempt_id").text() != Some(binding.text("root_owner"))
            || found.get("task_id").text() != Some(binding.text("task_id"))
        {
            return Err(err("Team lead lifecycle binding differs"));
        }
        let created = events
            .iter()
            .find(|v| {
                v.get("attempt_id").text() == Some(&attempt)
                    && v.get("event_type").text() == Some("attempt.created")
            })
            .and_then(|v| v.get("event_id").text())
            .ok_or_else(|| err("Team lead creation generation unavailable"))?
            .to_owned();
        drop(guard);
        let participant = format!("team-{}", identity.call_id);
        let room = oms_command_room::Room::open(
            selected.state_root,
            oms_command_room::Session::from_env(),
            service,
            stop,
            false,
            false,
        )
        .map_err(|e| err(e.message))?
        .with_deadline(deadline);
        let mut join = oms_command_room::Join::new(&participant, selected.provider);
        join.role = "advisor".into();
        join.parent = Some(binding.text("root_owner").into());
        // Room task declarations are writer scopes; the read-only lead's task
        // remains bound by its actual lifecycle and canonical Team contract.
        join.task_id = None;
        room.join(binding.text("room_id"), join)
            .map_err(|e| err(e.message))?;
        let state = oms_command_room::project(
            &room
                .records(binding.text("room_id"))
                .map_err(|e| err(e.message))?,
        )
        .map_err(|e| err(e.message))?;
        let enrollment = state
            .list("participants")
            .iter()
            .find(|v| v.text("participant") == participant)
            .map(|p| p.number("seq") as i128)
            .ok_or_else(|| err("Team lead Room enrollment unavailable"))?;
        let binding_sha = json::hash(&binding).map_err(plan_error)?;
        Ok(Self {
            service,
            context,
            admission,
            binding,
            binding_sha,
            attempt,
            created,
            participant,
            enrollment,
            call: identity.call_id.clone(),
            operation: identity.operation_id.clone(),
            deadline,
            expires_at,
            stop,
            controller: oms_reference::controller::Controller::capture_current().map_err(err)?,
            owner: None,
            session: String::new(),
            nonce: String::new(),
            generation: 0,
            event_sequence: 0,
            sequence: 0,
            hello: false,
            buffer: vec![],
            responses: vec![],
            consumed: 0,
            actions: launch.actions().to_vec(),
            private: None,
            issuance_stage: IssuanceStage::Unavailable,
        })
    }
    fn start_owned(&mut self, reactor: &mut Reactor, job: JobId) -> Result<()> {
        self.issuance_stage = IssuanceStage::ProcessSnapshot;
        let snapshot = reactor.snapshot(job).map_err(err)?;
        self.issuance_stage = IssuanceStage::NativeIdentity;
        self.owner = Some(
            NativeIdentity
                .owner_identity(snapshot.pid)
                .map_err(|_| err("Team child native generation unavailable"))?,
        );
        self.issuance_stage = IssuanceStage::DuplexConnection;
        self.generation = snapshot
            .duplex
            .ok_or_else(|| err("Team child has no duplex connection"))?
            .generation;
        self.issuance_stage = IssuanceStage::SessionRandomness;
        self.session = oms_command_plan::storage::random_hex(16).map_err(plan_error)?;
        self.nonce = oms_command_plan::storage::random_hex(32).map_err(plan_error)?;
        self.issuance_stage = IssuanceStage::ControllerCheck;
        self.controller.check().map_err(err)?;
        let owner = self.owner.as_ref().unwrap();
        self.issuance_stage = IssuanceStage::SessionRecord;
        let record = object([
            ("schema", number(1)),
            ("kind", string("oms-team-request-session")),
            ("session", string(&self.session)),
            ("nonce_sha256", string(sha(self.nonce.as_bytes()))),
            (
                "controller_sha256",
                string(self.controller.digest().map_err(err)?),
            ),
            ("call_id", string(&self.call)),
            ("operation_id", string(&self.operation)),
            ("attempt_id", string(&self.attempt)),
            ("attempt_generation", string(&self.created)),
            ("participant", string(&self.participant)),
            ("room_enrollment", number(self.enrollment)),
            ("binding_sha256", string(&self.binding_sha)),
            ("connection_generation", number(self.generation as i128)),
            ("pid", number(owner.pid as i128)),
            ("process_birth", string(&owner.birth)),
            (
                "allowed_actions",
                Value::Array(
                    self.actions
                        .iter()
                        .map(|action| string(action.name()))
                        .collect(),
                ),
            ),
        ]);
        let raw = json::canonical(&record).map_err(plan_error)?;
        self.issuance_stage = IssuanceStage::PrivateStore;
        let private = oms_panel_core::terminals::controller_store(&self.context.state_repo, true)
            .map_err(|e| err(e.message))?;
        let relative = PathBuf::from(format!("team-request-{}/session.json", self.session));
        let wall_remaining = u64::try_from(self.expires_at)
            .ok()
            .and_then(|seconds| std::time::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
            .and_then(|expiry| expiry.duration_since(std::time::SystemTime::now()).ok())
            .unwrap_or_default();
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(wall_remaining);
        self.issuance_stage = IssuanceStage::IssuanceLock;
        let locked = private
            .lock_at(
                Path::new("team-request-issuance.lock"),
                Duration::from_secs(1).min(remaining),
            )
            .map_err(|e| err(e.message))?;
        self.issuance_stage = IssuanceStage::PrivatePublication;
        locked
            .write(&relative, &raw, true, 8192)
            .map_err(|e| err(e.message))?;
        drop(locked);
        self.issuance_stage = IssuanceStage::PrivateBinding;
        self.private = Some(PrivateBinding::open(private.repo().join(relative), raw)?);
        self.issuance_stage = IssuanceStage::AdmissionRecheck;
        let expires_at = self.expiry()?;
        self.issuance_stage = IssuanceStage::HelloDelivery;
        let mut hello = object([
            ("schema", number(1)),
            ("kind", string("hello")),
            ("session", string(&self.session)),
            ("nonce", string(&self.nonce)),
            ("connection_generation", number(self.generation as i128)),
            ("binding_sha256", string(&self.binding_sha)),
            ("expires_at", number(expires_at)),
        ]);
        if self.actions != [TeamRequestAction::ReportBlocker] {
            hello
                .set(
                    "allowed_actions",
                    Value::Array(
                        self.actions
                            .iter()
                            .map(|action| string(action.name()))
                            .collect(),
                    ),
                )
                .map_err(plan_error)?;
            hello
                .set("task_id", self.binding.get("task_id").clone())
                .map_err(plan_error)?;
            hello
                .set("task_sha256", self.binding.get("task_sha256").clone())
                .map_err(plan_error)?;
        }
        self.send(reactor, job, &hello)
    }
    fn expiry(&self) -> Result<i128> {
        let plan = Plan::open(self.context.clone(), &self.service.control, self.stop)
            .map_err(plan_error)?;
        let guard = plan
            .team_admission_before(
                self.admission.clone(),
                self.deadline.min(Instant::now() + Duration::from_secs(1)),
            )
            .map_err(plan_error)?;
        if json::hash(guard.binding()).map_err(plan_error)? != self.binding_sha {
            return Err(err("Team channel contract changed"));
        }
        Ok(
            (guard.expires_at_unix().map_err(plan_error)?.unwrap_or(0) as i128)
                .min(self.expires_at),
        )
    }
    fn send(&self, reactor: &mut Reactor, job: JobId, value: &Value) -> Result<()> {
        let mut bytes = json::canonical(value).map_err(plan_error)?;
        bytes.push(b'\n');
        reactor
            .send(job, &bytes)
            .map_err(|_| err("Team response queue unavailable; durable request retained"))
    }
    fn output_event(
        &mut self,
        reactor: &mut Reactor,
        job: JobId,
        event: OutputEvent,
    ) -> Result<()> {
        if event.generation != self.generation || event.sequence != self.event_sequence + 1 {
            return Err(err("Team channel output gap"));
        }
        self.event_sequence = event.sequence;
        if event.stream == OutputStream::Stderr {
            return Ok(());
        }
        for byte in event.bytes {
            if byte == b'\n' {
                let bytes = std::mem::take(&mut self.buffer);
                self.frame(reactor, job, &bytes)?;
            } else {
                if self.buffer.len() >= 8192 {
                    return Err(err("Team protocol frame exceeds bound"));
                }
                self.buffer.push(byte);
            }
        }
        Ok(())
    }
    fn frame(&mut self, reactor: &mut Reactor, job: JobId, bytes: &[u8]) -> Result<()> {
        let value = json::parse(bytes).map_err(|_| err("malformed Team protocol frame"))?;
        let kind = value.text("kind");
        let fields = match kind {
            "hello" => vec![
                "schema",
                "kind",
                "session",
                "nonce",
                "connection_generation",
            ],
            "request" => vec![
                "schema",
                "kind",
                "session",
                "nonce",
                "connection_generation",
                "sequence",
                "binding_sha256",
                "body",
            ],
            "ack" => vec![
                "schema",
                "kind",
                "session",
                "nonce",
                "connection_generation",
                "sequence",
                "request_id",
                "receipt_challenge",
            ],
            _ => return Err(err("unsupported Team protocol frame")),
        };
        json::keys(&value, &fields, &[]).map_err(|_| err("unknown Team protocol field"))?;
        if !value.get("schema").is_int(1)
            || value.text("session") != self.session
            || value.text("nonce") != self.nonce
            || !value
                .get("connection_generation")
                .is_int(self.generation as i128)
        {
            return Err(err("Team channel authentication refused"));
        }
        if kind == "hello" {
            if self.hello {
                return Err(err("duplicate Team handshake"));
            }
            self.hello = true;
            return Ok(());
        }
        if !self.hello {
            return Err(err("Team handshake required"));
        }
        if kind == "ack" {
            let Some((id, seq, challenge)) = self.responses.get(self.consumed) else {
                return Err(err("Team acknowledgement precedes response"));
            };
            if value.text("request_id") != id
                || !value.get("sequence").is_int(*seq)
                || value.text("receipt_challenge") != challenge
            {
                return Err(err(
                    "Team acknowledgement does not match delivered response",
                ));
            }
            self.consumed += 1;
            return Ok(());
        }
        if self.sequence >= 32 || !value.get("sequence").is_int(self.sequence + 1) {
            return Err(err("Team request sequence or count refused"));
        }
        if value.text("binding_sha256") != self.binding_sha {
            return Err(err("Team request names another contract"));
        }
        if !self
            .actions
            .iter()
            .any(|action| action.name() == value.get("body").text("action"))
        {
            return Err(err("Team request action was not enabled by its parent"));
        }
        self.controller.check().map_err(err)?;
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| err("Team child ownership unavailable"))?;
        if NativeIdentity.owner_identity(owner.pid).ok().as_ref() != Some(owner) {
            return Err(err("Team child native generation changed"));
        }
        let private = self
            .private
            .as_ref()
            .ok_or_else(|| err("Team private operation binding unavailable"))?;
        private.check()?;
        if json::canonical(value.get("body"))
            .map_err(plan_error)?
            .windows(self.nonce.len())
            .any(|part| {
                part == self.nonce.as_bytes()
                    || self
                        .responses
                        .iter()
                        .any(|(_, _, secret)| part == secret.as_bytes())
            })
        {
            return Err(err("Team request contains private channel material"));
        }
        // Retain the exact selected contract as data; it never authenticates a sender.
        if self.binding.text("root_owner") != self.context.panel_main_attempt {
            return Err(err("Team root controller changed"));
        }
        let expiry = self.expiry()?;
        if now() >= expiry {
            return Err(err("Team channel expired"));
        }
        if value
            .get("body")
            .get("expires_at")
            .int()
            .map_err(plan_error)?
            > self.expires_at
        {
            return Err(err("Team request exceeds channel expiry"));
        }
        let plan = Plan::open(self.context.clone(), &self.service.control, self.stop)
            .map_err(plan_error)?;
        let outcome = plan
            .record_team_request_before(
                self.admission.clone(),
                &self.binding_sha,
                value.get("body"),
                &oms_command_plan::team_request::RequestEnrollment {
                    attempt: &self.attempt,
                    created: &self.created,
                    participant: &self.participant,
                    room_sequence: self.enrollment,
                    private_binding_sha256: &sha(&private.raw),
                },
                self.deadline.min(Instant::now() + Duration::from_secs(1)),
            )
            .map_err(plan_error)?;
        self.sequence += 1;
        let challenge = oms_command_plan::storage::random_hex(32).map_err(plan_error)?;
        self.responses.push((
            value.get("body").text("request_id").into(),
            self.sequence,
            challenge.clone(),
        ));
        self.send(
            reactor,
            job,
            &object([
                ("schema", number(1)),
                ("kind", string("response")),
                ("sequence", number(self.sequence)),
                ("request_id", value.get("body").get("request_id").clone()),
                ("outcome", outcome),
                ("receipt_challenge", string(&challenge)),
            ]),
        )
    }
    fn settle_enrollment(&self, proof: &oms_supervisor::process::OwnedCleanup) -> Result<()> {
        let call = oms_supervisor::process::PhysicalCall::new(&self.call).map_err(err)?;
        if ![
            oms_supervisor::process::Scope::ProcessGroup,
            oms_supervisor::process::Scope::WholeTree,
        ]
        .iter()
        .any(|scope| proof.matches(&call, *scope))
            || self
                .owner
                .as_ref()
                .is_none_or(|owner| owner.pid != proof.pid())
        {
            return Err(err("Team enrollment cleanup proof does not match"));
        }
        self.controller.check().map_err(err)?;
        self.private
            .as_ref()
            .ok_or_else(|| err("private Team operation unavailable"))?
            .check()?;
        let settling = std::sync::Arc::new(AtomicBool::new(false));
        let cleanup = CollabServices::new(&settling, &self.service.source).map_err(err)?;
        let room = oms_command_room::Room::open(
            &self.context.state_repo,
            oms_command_room::Session::from_env(),
            &cleanup,
            &settling,
            false,
            false,
        )
        .map_err(|e| err(e.message))?
        .with_deadline(Instant::now() + Duration::from_secs(5));
        room.leave_if_enrolled(
            self.binding.text("room_id"),
            &self.participant,
            self.enrollment
                .try_into()
                .map_err(|_| err("invalid Team enrollment generation"))?,
            self.binding.text("root_owner"),
        )
        .map_err(|e| err(e.message))?;
        Ok(())
    }
    pub(super) fn complete(&self) -> bool {
        self.hello
            && !self.responses.is_empty()
            && self.consumed == self.responses.len()
            && self.buffer.is_empty()
    }
    /// Failed channel conditions, without frames, ids or paths. A partial
    /// frame is reported independently of the phase that was reached.
    fn channel_cues(&self) -> Vec<&'static str> {
        let mut cues = Vec::new();
        if !self.hello {
            cues.push("channel-no-hello");
        } else if self.responses.is_empty() {
            cues.push("channel-no-request");
        } else if self.consumed != self.responses.len() {
            cues.push("channel-unconsumed-response");
        }
        if !self.buffer.is_empty() {
            cues.push("channel-partial-frame");
        }
        cues
    }
}

/// Nonsecret failed-condition names, in the order `complete` evaluates them.
fn incomplete_cues(
    enrollment_settled: bool,
    protocol_failed: bool,
    stopped: bool,
    output_complete: bool,
    cleanup_complete: bool,
    channel: &[&'static str],
    exit: Option<i32>,
) -> String {
    let mut cues = Vec::new();
    if !enrollment_settled {
        cues.push("enrollment-unsettled");
    }
    if protocol_failed {
        cues.push("protocol-failed");
    }
    if stopped {
        cues.push("supervisor-stopped");
    }
    if !output_complete {
        cues.push("output-incomplete");
    }
    if !cleanup_complete {
        cues.push("cleanup-incomplete");
    }
    cues.extend_from_slice(channel);
    let exit = exit.map_or("none".to_owned(), |c| c.to_string());
    format!("cause={}; exit={exit}", cues.join(","))
}
impl super::OwnedDuplexConsumer for Channel<'_, '_> {
    fn issuance_stage(&self) -> IssuanceStage {
        self.issuance_stage
    }

    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline || now() >= self.expires_at {
            Err(Error::new(124, "Team request channel expired"))
        } else {
            Ok(())
        }
    }

    fn started(&mut self, reactor: &mut Reactor, job: JobId) -> Result<()> {
        self.start_owned(reactor, job)
    }
    fn event(&mut self, reactor: &mut Reactor, job: JobId, event: OutputEvent) -> Result<()> {
        self.output_event(reactor, job, event)
    }
}
impl CollabServices<'_> {
    pub(super) fn team_requests(
        &self,
        mut input: Invocation,
        binding: TeamLaunchBinding<'_>,
        stop: &AtomicBool,
    ) -> Result<Completed> {
        if input.physical_call.as_ref().map(|call| call.id())
            != Some(binding.identity().call_id.as_str())
            || std::fs::canonicalize(&input.cwd)?
                != std::fs::canonicalize(binding.attempt().execution_repo)?
        {
            return Err(err("Team invocation differs from its New launch binding"));
        }
        let deadline = Instant::now() + input.timeout;
        let mut channel = Channel::prepare(self, &binding, stop, deadline)
            .map_err(|e| prelaunch_error(e, stop, deadline, None))?;
        let expires_at = channel.expires_at;
        let classify = |e| prelaunch_error(e, stop, deadline, Some(expires_at));
        binding.check(self, stop).map_err(classify)?;
        input.input = None;
        let argv = self
            .resolve_argv(input.argv)
            .map_err(err)
            .map_err(classify)?;
        if let Some(fence) = &input.launch_fence {
            fence.check(stop).map_err(classify)?;
        }
        binding.check(self, stop).map_err(classify)?;
        let (done, protocol_failed) = supervise(
            Job {
                stdin: None,
                environment: input.environment,
                remove_environment: input.remove_environment,
                merged: false,
                physical_call: input.physical_call,
                channel: Some(&mut channel),
                launch_fence: input.launch_fence.as_ref(),
                ..Job::new(
                    argv,
                    &input.cwd,
                    input.timeout,
                    input.max_bytes.min(512 * 1024),
                )
            },
            stop,
        )
        .map_err(err)
        .map_err(classify)?;
        let enrollment_settled = done
            .owned_cleanup()
            .is_some_and(|proof| channel.settle_enrollment(proof).is_ok());
        let output_complete = done.output_complete();
        let cleanup_complete = done.cleanup_complete();
        let complete = enrollment_settled
            && !protocol_failed
            && done.stop.is_none()
            && output_complete
            && cleanup_complete
            && channel.complete();
        // Protocol bytes include the private nonce. They never become an outward
        // provider artifact, even when the child prints a frame on stderr.
        Ok(Completed {
            code: if done.stop.is_some() {
                super::stop_code(&done)
            } else if complete {
                done.exit.map_or(75, |e| e.shell_code())
            } else {
                75
            },
            stdout: if complete {
                if channel.actions == [TeamRequestAction::ReportBlocker] {
                    b"Team blocker report recorded\n".to_vec()
                } else {
                    b"Team requests recorded\n".to_vec()
                }
            } else {
                vec![]
            },
            stderr: if complete {
                vec![]
            } else {
                format!(
                    "Team report channel incomplete; reconcile durable request receipts ({})\n",
                    incomplete_cues(
                        enrollment_settled,
                        protocol_failed,
                        done.stop.is_some(),
                        output_complete,
                        cleanup_complete,
                        &channel.channel_cues(),
                        done.exit.map(|e| e.shell_code()),
                    )
                )
                .into_bytes()
            },
            complete,
            cleanup_complete: Some(done.cleanup_complete()),
            owned_cleanup: done.owned_cleanup().cloned(),
            elapsed: done.duration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::incomplete_cues;

    #[test]
    fn incomplete_cues_name_each_failed_condition_only() {
        assert_eq!(
            incomplete_cues(
                true,
                false,
                false,
                true,
                true,
                &["channel-no-hello", "channel-partial-frame"],
                Some(0)
            ),
            "cause=channel-no-hello,channel-partial-frame; exit=0"
        );
        assert_eq!(
            incomplete_cues(false, true, true, false, false, &[], None),
            "cause=enrollment-unsettled,protocol-failed,supervisor-stopped,output-incomplete,cleanup-incomplete; exit=none"
        );
    }
}
