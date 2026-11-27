//! Native-owned read-only request channel. Public locators and capability declarations
//! select data; only this actual New/child/pipe issuer creates a session.
use super::{CollabServices, Job, peer_activity_heartbeat, prelaunch_error, supervise_flags};
use oms_command_peer::process::{
    Completed, Invocation, NATIVE_ADMISSION_BUDGET, TeamLaunchBinding, TeamRequestAction,
};
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

// Derive channel bounds from the 10 s NATIVE_ADMISSION_BUDGET: hold equals the
// full 10 s admission budget and lock wait takes 1/5 (2 s) across each writer
// lock set. Each bounded critical section (`issuance`, `expiry`, `record`) can
// block at most 2 s + 10 s = 12 s, so `start_owned` (`issuance` + `expiry`) and
// one request frame in the supervisor loop (`expiry` + `record`) each block at
// most 24 s (always capped by `deadline` and `expires_at`), far below the 900 s
// (15 m) default `peer-ask` call timeout.
const CHANNEL_LOCK_HOLD_BOUND: Duration = NATIVE_ADMISSION_BUDGET;
const CHANNEL_LOCK_WAIT_BOUND: Duration =
    Duration::from_millis((CHANNEL_LOCK_HOLD_BOUND.as_millis() / 5) as u64);

fn channel_admission_bounds() -> oms_command_plan::team::AdmissionBounds {
    oms_command_plan::team::AdmissionBounds {
        lock_wait: CHANNEL_LOCK_WAIT_BOUND,
        lock_hold: CHANNEL_LOCK_HOLD_BOUND,
    }
}

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

// The underlying custody and storage errors can include private details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnrollmentFailureStage {
    MissingCleanupProof,
    CleanupProofMismatch,
    ControllerCheck,
    PrivateBinding,
    RoomSetup,
    CheckedRoomLeave,
}
impl EnrollmentFailureStage {
    fn name(self) -> &'static str {
        match self {
            Self::MissingCleanupProof => "missing-cleanup-proof",
            Self::CleanupProofMismatch => "cleanup-proof-mismatch",
            Self::ControllerCheck => "controller-check",
            Self::PrivateBinding => "private-binding",
            Self::RoomSetup => "room-setup",
            Self::CheckedRoomLeave => "checked-room-leave",
        }
    }
}

fn with_settlement_note(mut error: Error, stage: EnrollmentFailureStage) -> Error {
    error.message = format!(
        "{} (enrollment-unsettled; enrollment-stage={})",
        error.message,
        stage.name()
    );
    error
}

fn with_retained_enrollment_note(mut error: Error, stage: EnrollmentFailureStage) -> Error {
    error.message = format!(
        "{} (room-enrollment-retained; enrollment-stage={})",
        error.message,
        stage.name()
    );
    error
}

fn classify_then_settle(
    error: Error,
    stop: &AtomicBool,
    deadline: Instant,
    expires_at: Option<i128>,
    settle: impl FnOnce() -> std::result::Result<(), EnrollmentFailureStage>,
) -> Error {
    let classified = prelaunch_error(error, stop, deadline, expires_at);
    match settle() {
        Ok(()) => classified,
        Err(stage) => with_settlement_note(classified, stage),
    }
}

fn with_settlement_room<T>(
    state_repo: &Path,
    action: impl FnOnce(&oms_command_room::Room<'_>) -> std::result::Result<T, EnrollmentFailureStage>,
) -> std::result::Result<T, EnrollmentFailureStage> {
    struct SettlementRoomServices<'a> {
        control: oms_command_autonomy::LocalServices<'a>,
        stop: &'a AtomicBool,
    }
    impl oms_command_room::Services for SettlementRoomServices<'_> {
        fn state_repository(&self, repo: &Path) -> oms_command_room::Result<PathBuf> {
            crate::services_control::state_repository_for(repo)
                .map_err(oms_command_room::Error::new)
        }
        fn canonical_provider(&self, provider: &str) -> oms_command_room::Result<String> {
            oms_command_room::Services::canonical_provider(
                &oms_command_room::LocalServices,
                provider,
            )
        }
        fn operations_before<'a>(
            &'a self,
            repo: &Path,
            deadline: Instant,
        ) -> oms_command_room::Result<Box<dyn oms_command_room::OperationsRead + 'a>> {
            super::room_operations_before(&self.control, repo, deadline, self.stop)
        }
    }
    let settling = AtomicBool::new(false);
    let cleanup = SettlementRoomServices {
        control: oms_command_autonomy::LocalServices::new(&settling),
        stop: &settling,
    };
    let room = oms_command_room::Room::open(
        state_repo,
        oms_command_room::Session::from_env(),
        &cleanup,
        &settling,
        false,
        false,
    )
    .map_err(|_| EnrollmentFailureStage::RoomSetup)?
    .with_deadline(Instant::now() + Duration::from_secs(5));
    action(&room)
}

fn settle_prepare_room_leave(
    state_repo: &Path,
    room_id: &str,
    participant: &str,
    root_owner: &str,
    check_controller: impl FnOnce() -> std::result::Result<(), EnrollmentFailureStage>,
) -> std::result::Result<(), EnrollmentFailureStage> {
    use EnrollmentFailureStage as Stage;
    with_settlement_room(state_repo, |room| {
        let rows = room.records(room_id).map_err(|_| Stage::RoomSetup)?;
        let state = oms_command_room::project(&rows).map_err(|_| Stage::RoomSetup)?;
        let Some(enrolled) = state
            .list("participants")
            .iter()
            .find(|v| v.text("participant") == participant && v.flag("joined"))
        else {
            return Ok(());
        };
        check_controller()?;
        room.leave_if_enrolled(room_id, participant, enrolled.number("seq"), root_owner)
            .map_err(|_| Stage::CheckedRoomLeave)?;
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn enroll_room_before<C, J, R>(
    state_repo: &Path,
    room_id: &str,
    participant: &str,
    root_owner: &str,
    stop: &AtomicBool,
    deadline: Instant,
    expires_at: i128,
    check_controller: C,
    join_op: J,
    records_op: R,
) -> Result<i128>
where
    C: Fn() -> std::result::Result<(), EnrollmentFailureStage>,
    J: FnOnce() -> oms_command_room::Result<()>,
    R: FnOnce() -> oms_command_room::Result<Vec<oms_command_room::value::Value>>,
{
    let attempt = (|| -> Result<i128> {
        join_op().map_err(|e| err(e.message))?;
        let rows = records_op().map_err(|e| err(e.message))?;
        let state = oms_command_room::project(&rows).map_err(|e| err(e.message))?;
        let enrollment = state
            .list("participants")
            .iter()
            .find(|v| v.text("participant") == participant && v.flag("joined"))
            .map(|p| p.number("seq") as i128)
            .ok_or_else(|| err("Team lead Room enrollment unavailable"))?;
        check_controller().map_err(|_| err("native executing-artifact generation changed"))?;
        Ok(enrollment)
    })();
    match attempt {
        Ok(enrollment) => Ok(enrollment),
        Err(error) => Err(classify_then_settle(
            error,
            stop,
            deadline,
            Some(expires_at),
            || {
                settle_prepare_room_leave(
                    state_repo,
                    room_id,
                    participant,
                    root_owner,
                    check_controller,
                )
            },
        )),
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
    controller: super::CheckedController,
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
    spawned: bool,
    issuance_stage: IssuanceStage,
}
impl<'a, 's> Channel<'a, 's> {
    fn prepare(
        service: &'a CollabServices<'s>,
        launch: &TeamLaunchBinding<'_>,
        stop: &'a AtomicBool,
        deadline: Instant,
    ) -> Result<Self> {
        let mut known_expiry = None;
        let pre_join = (|| -> Result<_> {
            let controller = service.controller().map_err(err)?;
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
            known_expiry = Some(expires_at);
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
            let binding_sha = json::hash(&binding).map_err(plan_error)?;
            drop(guard);
            if Instant::now() >= deadline || now() >= expires_at {
                return Err(Error::new(124, "Team request channel expired"));
            }
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
            Ok((
                controller,
                selected,
                identity,
                context,
                admission,
                binding,
                binding_sha,
                expires_at,
                attempt,
                created,
                participant,
                room,
            ))
        })();
        let (
            controller,
            selected,
            identity,
            context,
            admission,
            binding,
            binding_sha,
            expires_at,
            attempt,
            created,
            participant,
            room,
        ) = pre_join.map_err(|e| prelaunch_error(e, stop, deadline, known_expiry))?;
        let mut join = oms_command_room::Join::new(&participant, selected.provider);
        join.role = "advisor".into();
        join.parent = Some(binding.text("root_owner").into());
        // Room task declarations are writer scopes; the read-only lead's task
        // remains bound by its actual lifecycle and canonical Team contract.
        join.task_id = None;
        let room_id = binding.text("room_id");
        let root_owner = binding.text("root_owner");
        let enrollment = enroll_room_before(
            &context.state_repo,
            room_id,
            &participant,
            root_owner,
            stop,
            deadline,
            expires_at,
            || {
                controller
                    .check()
                    .map_err(|_| EnrollmentFailureStage::ControllerCheck)
            },
            || room.join(room_id, join).map(|_| ()),
            || room.records(room_id),
        )?;
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
            controller,
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
            spawned: false,
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
        self.issuance_stage = IssuanceStage::IssuanceLock;
        let (window_deadline, bounds) = self.bounded_window();
        let locked = private
            .lock_at_bounded(
                Path::new("team-request-issuance.lock"),
                self.stop,
                window_deadline,
                bounds,
            )
            .map_err(|e| Error::new(e.code, e.message))?;
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
    fn bounded_window(&self) -> (Instant, oms_command_plan::team::AdmissionBounds) {
        let window_deadline = if self.expires_at <= 0 {
            Instant::now()
        } else if let Ok(seconds) = u64::try_from(self.expires_at)
            && let Some(expiry) = std::time::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
        {
            let wall_remaining = expiry
                .duration_since(std::time::SystemTime::now())
                .unwrap_or_default();
            Instant::now()
                .checked_add(wall_remaining)
                .map_or(self.deadline, |wall_deadline| {
                    self.deadline.min(wall_deadline)
                })
        } else {
            self.deadline
        };
        (window_deadline, channel_admission_bounds())
    }
    fn expiry(&self) -> Result<i128> {
        let plan = Plan::open(self.context.clone(), &self.service.control, self.stop)
            .map_err(plan_error)?;
        let (window_deadline, bounds) = self.bounded_window();
        let guard = plan
            .team_admission_bounded(self.admission.clone(), window_deadline, bounds)
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
        if Instant::now() >= self.deadline || now() >= expiry {
            return Err(Error::new(124, "Team channel expired"));
        }
        if value
            .get("body")
            .get("expires_at")
            .int()
            .map_err(plan_error)?
            > expiry
        {
            return Err(err("Team request exceeds channel expiry"));
        }
        let plan = Plan::open(self.context.clone(), &self.service.control, self.stop)
            .map_err(plan_error)?;
        let (window_deadline, bounds) = self.bounded_window();
        let outcome = plan
            .record_team_request_bounded(
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
                window_deadline,
                bounds,
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
    fn settle_room_leave(&self) -> std::result::Result<(), EnrollmentFailureStage> {
        use EnrollmentFailureStage as Stage;
        let sequence = self
            .enrollment
            .try_into()
            .map_err(|_| Stage::CheckedRoomLeave)?;
        with_settlement_room(&self.context.state_repo, |room| {
            room.leave_if_enrolled(
                self.binding.text("room_id"),
                &self.participant,
                sequence,
                self.binding.text("root_owner"),
            )
            .map_err(|_| Stage::CheckedRoomLeave)?;
            Ok(())
        })
    }
    fn settle_unspawned_enrollment(&self) -> std::result::Result<(), EnrollmentFailureStage> {
        if self.spawned {
            return Err(EnrollmentFailureStage::MissingCleanupProof);
        }
        self.controller
            .check()
            .map_err(|_| EnrollmentFailureStage::ControllerCheck)?;
        self.settle_room_leave()
    }
    fn fail_before_spawn(&self, error: Error) -> Error {
        classify_then_settle(
            error,
            self.stop,
            self.deadline,
            Some(self.expires_at),
            || self.settle_unspawned_enrollment(),
        )
    }
    fn fail_after_spawn(&self, error: Error) -> Error {
        // Once `reactor.start` succeeds, leaving Room requires an `OwnedCleanup`
        // proof that the process tree is quiescent; retain enrollment explicitly.
        with_retained_enrollment_note(
            prelaunch_error(error, self.stop, self.deadline, Some(self.expires_at)),
            EnrollmentFailureStage::MissingCleanupProof,
        )
    }
    fn settle_enrollment(
        &self,
        proof: &oms_supervisor::process::OwnedCleanup,
    ) -> std::result::Result<(), EnrollmentFailureStage> {
        use EnrollmentFailureStage as Stage;
        let call = oms_supervisor::process::PhysicalCall::new(&self.call)
            .map_err(|_| Stage::CleanupProofMismatch)?;
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
            return Err(Stage::CleanupProofMismatch);
        }
        self.controller
            .check()
            .map_err(|_| Stage::ControllerCheck)?;
        self.private
            .as_ref()
            .ok_or(Stage::PrivateBinding)?
            .check()
            .map_err(|_| Stage::PrivateBinding)?;
        self.settle_room_leave()
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
    enrollment_failure: Option<EnrollmentFailureStage>,
    protocol_failed: bool,
    stopped: bool,
    output_complete: bool,
    cleanup_complete: bool,
    channel: &[&'static str],
    exit: Option<i32>,
) -> String {
    let mut cues = Vec::new();
    if enrollment_failure.is_some() {
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
    let mut detail = format!("cause={}; exit={exit}", cues.join(","));
    if let Some(stage) = enrollment_failure {
        detail.push_str("; enrollment-stage=");
        detail.push_str(stage.name());
    }
    detail
}
impl super::OwnedDuplexConsumer for Channel<'_, '_> {
    fn note_spawned(&mut self) {
        self.spawned = true;
        #[cfg(test)]
        self.service.record_team_child_spawn();
    }

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
        let mut channel = Channel::prepare(self, &binding, stop, deadline)?;
        #[cfg(test)]
        self.run_after_team_room_join(&channel.participant);
        if let Err(error) = binding.check(self, stop) {
            return Err(channel.fail_before_spawn(error));
        }
        input.input = None;
        let argv = match self.resolve_argv(input.argv) {
            Ok(argv) => argv,
            Err(message) => return Err(channel.fail_before_spawn(err(message))),
        };
        if let Some(fence) = &input.launch_fence
            && let Err(error) = fence.check(stop)
        {
            return Err(channel.fail_before_spawn(error));
        }
        if let Err(error) = binding.check(self, stop) {
            return Err(channel.fail_before_spawn(error));
        }
        let heartbeat = peer_activity_heartbeat(input.heartbeat.as_ref());
        // A failed activity heartbeat is observability, not a protocol failure.
        let (done, _, protocol_failed) = match supervise_flags(
            Job {
                stdin: None,
                heartbeat: heartbeat.as_ref(),
                environment: input.environment,
                remove_environment: input.remove_environment,
                merged: false,
                physical_call: input.physical_call,
                channel: Some(&mut channel),
                launch_fence: input.launch_fence.as_ref(),
                plan_services: Some(&self.control),
                ..Job::new(
                    argv,
                    &input.cwd,
                    input.timeout,
                    input.max_bytes.min(512 * 1024),
                )
            },
            stop,
        ) {
            Ok(done) => done,
            Err(message) => {
                let error = err(message);
                return Err(if channel.spawned {
                    channel.fail_after_spawn(error)
                } else {
                    channel.fail_before_spawn(error)
                });
            }
        };
        let enrollment_failure = done
            .owned_cleanup()
            .ok_or(EnrollmentFailureStage::MissingCleanupProof)
            .and_then(|proof| channel.settle_enrollment(proof))
            .err();
        let output_complete = done.output_complete();
        let cleanup_complete = done.cleanup_complete();
        let complete = enrollment_failure.is_none()
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
                        enrollment_failure,
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
    use super::{
        EnrollmentFailureStage, classify_then_settle, enroll_room_before, err, incomplete_cues,
        now, with_retained_enrollment_note,
    };
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    #[test]
    fn incomplete_cues_name_each_failed_condition_only() {
        assert_eq!(
            incomplete_cues(
                None,
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
            incomplete_cues(
                Some(EnrollmentFailureStage::MissingCleanupProof),
                true,
                true,
                false,
                false,
                &[],
                None
            ),
            "cause=enrollment-unsettled,protocol-failed,supervisor-stopped,output-incomplete,cleanup-incomplete; exit=none; enrollment-stage=missing-cleanup-proof"
        );
        use EnrollmentFailureStage as Stage;
        for (stage, label) in [
            (Stage::MissingCleanupProof, "missing-cleanup-proof"),
            (Stage::CleanupProofMismatch, "cleanup-proof-mismatch"),
            (Stage::ControllerCheck, "controller-check"),
            (Stage::PrivateBinding, "private-binding"),
            (Stage::RoomSetup, "room-setup"),
            (Stage::CheckedRoomLeave, "checked-room-leave"),
        ] {
            assert_eq!(
                incomplete_cues(Some(stage), false, false, true, true, &[], Some(0)),
                format!("cause=enrollment-unsettled; exit=0; enrollment-stage={label}")
            );
        }
    }

    #[test]
    fn prelaunch_settlement_preserves_prior_classification_and_notes_failures() {
        let stop = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_millis(25);
        let settled = classify_then_settle(
            err("Team policy changed"),
            &stop,
            deadline,
            Some(now() + 60),
            || {
                std::thread::sleep(Duration::from_millis(50));
                Ok(())
            },
        );
        assert_eq!(
            settled.code, 75,
            "deadline passing during settlement must not turn exit 75 into 124"
        );
        assert_eq!(settled.message, "Team policy changed");

        let unnoted_deadline = Instant::now() + Duration::from_millis(25);
        let failed = classify_then_settle(
            err("Team policy changed"),
            &stop,
            unnoted_deadline,
            Some(now() + 60),
            || {
                std::thread::sleep(Duration::from_millis(50));
                Err(EnrollmentFailureStage::CheckedRoomLeave)
            },
        );
        assert_eq!(failed.code, 75);
        assert_eq!(
            failed.message,
            "Team policy changed (enrollment-unsettled; enrollment-stage=checked-room-leave)"
        );

        let already_expired = classify_then_settle(
            err("Team policy changed"),
            &stop,
            Instant::now(),
            Some(now() + 60),
            || Err(EnrollmentFailureStage::RoomSetup),
        );
        assert_eq!(already_expired.code, 124);
        assert_eq!(
            already_expired.message,
            "Team policy changed (enrollment-unsettled; enrollment-stage=room-setup)"
        );

        let retained = with_retained_enrollment_note(
            err("Team channel issuance failed; pending intent retained (stage=issuance-lock)"),
            EnrollmentFailureStage::MissingCleanupProof,
        );
        assert_eq!(retained.code, 75);
        assert_eq!(
            retained.message,
            "Team channel issuance failed; pending intent retained (stage=issuance-lock) (room-enrollment-retained; enrollment-stage=missing-cleanup-proof)"
        );
    }

    #[test]
    fn prepare_enrollment_leaves_room_on_every_post_join_failure() {
        use oms_command_operations::value::obj;
        use oms_command_operations::{StartRequest, append_lifecycle, create_attempt, new_event};
        use std::os::unix::fs::PermissionsExt;

        struct TempRepo(std::path::PathBuf);
        impl Drop for TempRepo {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        const CHILD_ENV: &str = "OMS_TEST_PREPARE_ENROLLMENT_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let home_dir = std::env::temp_dir()
                .join(format!("oms-team-req-home-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&home_dir).unwrap();
            std::fs::set_permissions(&home_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let _cleanup = TempRepo(home_dir.clone());
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "services_collab::team_requests::tests::prepare_enrollment_leaves_room_on_every_post_join_failure",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env("HOME", &home_dir)
            .env("XDG_STATE_HOME", &home_dir)
            .env("OMS_LOCK_DIR", home_dir.join("locks"));
            for key in [
                "OMS_STATE_REPO",
                "OMS_HARNESS_CHILD",
                "OMS_HARNESS_DELEGATE_DEPTH",
                "OMS_ROOM_ID",
                "OMS_ROOM_PARTICIPANT",
            ] {
                cmd.env_remove(key);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let raw_dir =
            std::env::temp_dir().join(format!("oms-team-req-prep-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&raw_dir).unwrap();
        let _cleanup = TempRepo(raw_dir.clone());
        let repo = std::fs::canonicalize(&raw_dir).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let collab = crate::services_collab::CollabServices::new(&stop, source).unwrap();
        let room = oms_command_room::Room::open(
            &repo,
            oms_command_room::Session {
                agent: "codex".into(),
                ..Default::default()
            },
            &collab,
            &stop,
            true,
            true,
        )
        .unwrap();
        room.create(
            Some("room-prep"),
            "Prepare cleanup fixture",
            None,
            None,
            None,
        )
        .unwrap();
        let owner = create_attempt(
            &repo,
            &StartRequest {
                provider: "codex".into(),
                tool: "panel-main".into(),
                refs: obj([
                    ("panel_room_id", "room-prep".into()),
                    ("panel_role", "main".into()),
                ]),
                ..Default::default()
            },
            &stop,
        )
        .unwrap();
        for state in ["starting", "working"] {
            let event = new_event(
                &owner,
                0usize.into(),
                "attempt.state_changed",
                obj([("to_state", state.into())]),
            )
            .unwrap();
            append_lifecycle(&repo, event, &stop).unwrap();
        }
        room.join("room-prep", oms_command_room::Join::new(&owner, "codex"))
            .unwrap();
        let pre_join_rows = room.records("room-prep").unwrap();

        let make_join = |participant: &str| {
            let mut join = oms_command_room::Join::new(participant, "codex");
            join.role = "advisor".into();
            join.parent = Some(owner.clone());
            join
        };
        let assert_not_joined = |participant: &str| {
            let state = oms_command_room::project(&room.records("room-prep").unwrap()).unwrap();
            assert!(
                state
                    .list("participants")
                    .iter()
                    .filter(|p| p.text("participant") == participant)
                    .all(|p| !p.flag("joined")),
                "{participant} stayed enrolled in Room after prepare failure"
            );
        };

        // 1. Join fails AFTER publishing the participant row.
        let p1 = "team-call_after_publish";
        let e1 = enroll_room_before(
            &repo,
            "room-prep",
            p1,
            &owner,
            &stop,
            Instant::now() + Duration::from_secs(5),
            now() + 60,
            || Ok(()),
            || {
                room.join("room-prep", make_join(p1))?;
                Err(oms_command_room::Error::new(
                    "Room append may already be published; reconcile: room deadline expired",
                ))
            },
            || room.records("room-prep"),
        )
        .unwrap_err();
        assert_eq!(e1.code, 75);
        assert!(!e1.message.contains("enrollment-unsettled"), "{e1:?}");
        assert_not_joined(p1);

        // 2. Records read fails after a successful join.
        let p2 = "team-call_records_err";
        let e2 = enroll_room_before(
            &repo,
            "room-prep",
            p2,
            &owner,
            &stop,
            Instant::now() + Duration::from_secs(5),
            now() + 60,
            || Ok(()),
            || room.join("room-prep", make_join(p2)).map(|_| ()),
            || Err(oms_command_room::Error::new("room log unavailable")),
        )
        .unwrap_err();
        assert_eq!(e2.code, 75);
        assert_not_joined(p2);

        // 3. Projection fails after a successful join.
        let p3 = "team-call_project_err";
        let bad_row =
            oms_command_room::value::parse(b"{\"schema\":99}\n", oms_command_room::MAX_ROW)
                .unwrap();
        let e3 = enroll_room_before(
            &repo,
            "room-prep",
            p3,
            &owner,
            &stop,
            Instant::now() + Duration::from_secs(5),
            now() + 60,
            || Ok(()),
            || room.join("room-prep", make_join(p3)).map(|_| ()),
            || Ok(vec![bad_row]),
        )
        .unwrap_err();
        assert_eq!(e3.code, 75);
        assert_not_joined(p3);

        // 4. Enrollment lookup fails after a successful join.
        let p4 = "team-call_lookup_err";
        let e4 = enroll_room_before(
            &repo,
            "room-prep",
            p4,
            &owner,
            &stop,
            Instant::now() + Duration::from_secs(5),
            now() + 60,
            || Ok(()),
            || room.join("room-prep", make_join(p4)).map(|_| ()),
            || Ok(pre_join_rows.clone()),
        )
        .unwrap_err();
        assert_eq!(e4.code, 75);
        assert!(e4.message.contains("enrollment unavailable"), "{e4:?}");
        assert_not_joined(p4);

        // 5. Settlement failure during prepare appends redacted stage note.
        let p5 = "team-call_settle_fail";
        let e5 = enroll_room_before(
            &repo,
            "room-prep",
            p5,
            &owner,
            &stop,
            Instant::now() + Duration::from_secs(5),
            now() + 60,
            || Err(EnrollmentFailureStage::ControllerCheck),
            || room.join("room-prep", make_join(p5)).map(|_| ()),
            || room.records("room-prep"),
        )
        .unwrap_err();
        assert_eq!(e5.code, 75);
        assert!(
            e5.message
                .ends_with("(enrollment-unsettled; enrollment-stage=controller-check)"),
            "{e5:?}"
        );
    }
}
