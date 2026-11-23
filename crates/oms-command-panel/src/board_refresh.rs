//! Coalesced Board reads, with exclusive transfer of the warm Session.
use crate::{Error, Result, Services, commands, identifier, require, rows};
use oms_panel_core::services::{CommandOutput, ExecutionRequest, PeerExecutor};
use oms_panel_core::{
    NativeCommand, SubcommandServices,
    metrics::{CodexQuotaSources, ReceiptTokensCache},
    native::operations::EventsProjectionCache,
    state::{self, AttemptCache, Snapshot},
    terminals::PROVIDER_SESSION_IDENTITY_FORMAT,
};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Instant;

#[derive(Clone, Debug)]
pub(crate) struct RefreshKey {
    repo: PathBuf,
    device: u64,
    inode: u64,
    room: Option<String>,
    main: Option<String>,
    member: Option<String>,
    launch_path: Option<PathBuf>,
}

impl PartialEq for RefreshKey {
    fn eq(&self, other: &Self) -> bool {
        (
            &self.repo,
            self.device,
            self.inode,
            &self.room,
            &self.main,
            &self.member,
        ) == (
            &other.repo,
            other.device,
            other.inode,
            &other.room,
            &other.main,
            &other.member,
        )
    }
}

impl Eq for RefreshKey {}

impl RefreshKey {
    /// `repo` is the Board's resolved state root, including for scratch worktrees.
    pub(crate) fn new(repo: &Path, room: Option<&str>, main: Option<&str>) -> Result<Self> {
        require(
            room.is_some() || main.is_none(),
            "Board Main requires a Room",
        )?;
        for id in room.into_iter().chain(main) {
            identifier(id)?;
        }
        let repo = repo.canonicalize().map_err(Error::new)?;
        let metadata = std::fs::symlink_metadata(&repo).map_err(Error::new)?;
        require(
            metadata.is_dir(),
            "Board state repository is not a directory",
        )?;
        Ok(Self {
            repo,
            device: metadata.dev(),
            inode: metadata.ino(),
            room: room.map(str::to_owned),
            main: main.map(str::to_owned),
            member: main.map(str::to_owned),
            launch_path: None,
        })
    }

    // A native swap retains the Room participant across Main attempt generations.
    pub(crate) fn with_member(mut self, member: Option<&str>) -> Result<Self> {
        require(
            member.is_some() == self.main.is_some(),
            "Board Main requires a Room participant",
        )?;
        if let Some(member) = member {
            identifier(member)?;
        }
        self.member = member.map(str::to_owned);
        Ok(self)
    }

    pub(crate) fn with_launch_path(mut self, path: Option<String>) -> Self {
        self.launch_path = path
            .filter(|path| {
                self.main.is_some() && path.len() <= 4096 && !path.chars().any(char::is_control)
            })
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        self
    }

    pub(crate) fn same_launch_path(&self, other: &Self) -> bool {
        self.launch_path == other.launch_path
    }

    fn current_repo(&self) -> bool {
        std::fs::symlink_metadata(&self.repo).is_ok_and(|metadata| {
            metadata.is_dir() && metadata.dev() == self.device && metadata.ino() == self.inode
        })
    }

    pub(crate) fn matches(&self, snapshot: &Snapshot) -> bool {
        let report = &snapshot.report;
        let room = &report["room"];
        if self.room.as_ref().is_some_and(|id| room["id"] != *id) {
            return false;
        }
        let Some(main) = &self.main else {
            return true;
        };
        let attempt = &report["room_attempts"][main];
        if room["closed"] != false
            || !room["successor"].is_null()
            || attempt["attempt_id"] != *main
            || attempt["tool"] != "panel-main"
            || attempt["refs"]["panel_role"] != "main"
            || attempt["refs"]["panel_room_id"] != room["id"]
            || attempt["terminal"] != false
            || !attempt["state"]
                .as_str()
                .is_some_and(|state| state::LIVE_STATES.contains(&state))
        {
            return false;
        }

        let Some(provider) = attempt["provider"]
            .as_str()
            .filter(|provider| identifier(provider).is_ok())
        else {
            return false;
        };
        let member = state::main_participant(attempt);
        if self.member.as_deref() != Some(member) {
            return false;
        }
        let mut mains = rows(&room["participants"]).iter().filter(|row| {
            row["participant"] == member || (row["role"] == "main" && row["joined"] == true)
        });
        mains.next().is_some_and(|row| {
            row["participant"] == member
                && row["provider"] == provider
                && row["role"] == "main"
                && row["joined"] == true
                && (row["parent"].is_null() || row["parent"] == "")
        }) && mains.next().is_none()
    }
}

struct Request {
    id: u64,
    key: RefreshKey,
    native: Option<crate::watch::NativeRequest>,
}

struct Job {
    request: Request,
    session: commands::Session,
}

struct Reply {
    job: Job,
    result: Result<(Snapshot, CodexQuotaSources)>,
    native: Option<crate::watch::NativeReading>,
}

pub(crate) struct BoardRefresh {
    sender: Option<Sender<Job>>,
    replies: Receiver<Reply>,
    exited: Receiver<()>,
    ready: Option<std::result::Result<Reply, TryRecvError>>,
    stop: Arc<AtomicBool>,
    idle: Option<commands::Session>,
    session_key: Option<RefreshKey>,
    accepted: bool,
    latest: u64,
    pending: Option<Request>,
    native: Option<crate::watch::NativeReading>,
}

impl BoardRefresh {
    pub(crate) fn start() -> Result<Self> {
        Self::spawn_with(|| {
            Ok(BoardCollector {
                commands: SubcommandServices::sibling()?,
                events: EventsProjectionCache::new(),
            })
        })
    }

    pub(crate) fn spawn_with<T: Services>(
        factory: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<Self> {
        let (sender, jobs) = mpsc::channel::<Job>();
        let (completed, replies) = mpsc::channel();
        let (worker_lifetime, exited) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let interrupted = stop.clone();
        std::thread::Builder::new()
            .name("oms-board-refresh".into())
            .spawn(move || {
                let _worker_lifetime = worker_lifetime;
                let service = factory();
                let mut receipts = ReceiptTokensCache::default();
                while let Ok(job) = jobs.recv() {
                    if interrupted.load(Ordering::Acquire) {
                        break;
                    }
                    let mut result = match &service {
                        Ok(service) => collect(&job, service, &interrupted, &mut receipts),
                        Err(error) => Err(error.clone()),
                    };
                    let native = job
                        .request
                        .native
                        .as_ref()
                        .zip(result.as_mut().ok())
                        .and_then(|(native, (snapshot, sources))| {
                            let adapter = SubcommandServices::sibling().ok()?;
                            native.observe(
                                snapshot,
                                sources,
                                &job.request.key.repo,
                                job.session.attempt_cache(),
                                &adapter,
                                &interrupted,
                            )
                        });
                    if interrupted.load(Ordering::Acquire)
                        || completed
                            .send(Reply {
                                job,
                                result,
                                native,
                            })
                            .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(Error::new)?;
        Ok(Self {
            sender: Some(sender),
            replies,
            exited,
            ready: None,
            stop,
            idle: Some(commands::Session::new()),
            session_key: None,
            accepted: false,
            latest: 0,
            pending: None,
            native: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn request(&mut self, key: RefreshKey) -> Result<u64> {
        self.request_with_native(key, None)
    }

    pub(crate) fn request_with_native(
        &mut self,
        key: RefreshKey,
        native: Option<crate::watch::NativeRequest>,
    ) -> Result<u64> {
        commands::clear_codex_quota_receipt();
        require(self.sender.is_some(), "Board refresh worker unavailable")?;
        self.latest = self
            .latest
            .checked_add(1)
            .ok_or_else(|| Error::new("Board refresh request sequence exhausted"))?;
        self.accepted = false;
        let request = Request {
            id: self.latest,
            key,
            native,
        };
        if let Some(session) = self.idle.take() {
            self.dispatch(request, session)?;
        } else {
            self.pending = Some(request);
        }
        Ok(self.latest)
    }

    fn dispatch(&mut self, request: Request, session: commands::Session) -> Result<()> {
        let session = if self
            .session_key
            .as_ref()
            .is_some_and(|key| key != &request.key)
        {
            commands::Session::new()
        } else {
            session
        };
        self.session_key = Some(request.key.clone());
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| Error::new("Board refresh worker unavailable"))?;
        if sender.send(Job { request, session }).is_err() {
            self.cancel();
            return Err(Error::new("Board refresh worker exited"));
        }
        Ok(())
    }

    pub(crate) fn busy(&self) -> bool {
        self.sender.is_some() && self.idle.is_none()
    }

    pub(crate) fn queued(&self) -> bool {
        self.pending.is_some()
    }

    /// Borrow only the accepted scope's idle cache. Dispatch takes it away entirely.
    pub(crate) fn idle_attempt_cache(&self, current: &RefreshKey) -> Option<&AttemptCache> {
        let session = self.idle.as_ref()?;
        (self.accepted
            && current.main.is_some()
            && self.session_key.as_ref() == Some(current)
            && current.current_repo())
        .then(|| session.attempt_cache())
    }

    /// Peek without accepting a reply so Watch only resamples tmux when needed.
    pub(crate) fn ready(&mut self) -> bool {
        if self.sender.is_some() && self.ready.is_none() {
            match self.replies.try_recv() {
                Err(TryRecvError::Empty) => {}
                reply => self.ready = Some(reply),
            }
        }
        self.ready.is_some()
    }

    pub(crate) fn poll(&mut self, current: &RefreshKey) -> Option<Result<Snapshot>> {
        commands::clear_codex_quota_receipt();
        self.sender.as_ref()?;
        self.native = None;
        let Reply {
            job,
            result,
            native,
        } = match self.ready.take().unwrap_or_else(|| self.replies.try_recv()) {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.cancel();
                return Some(Err(Error::new("Board refresh worker exited")));
            }
        };
        let current_repo = current.current_repo();
        if let Some(pending) = self.pending.take()
            && pending.key == *current
            && current_repo
        {
            return self.dispatch(pending, job.session).err().map(Err);
        }
        self.idle = Some(job.session);
        if job.request.id != self.latest || job.request.key != *current || !current_repo {
            return None;
        }
        self.accepted = result.as_ref().is_ok_and(|(snapshot, _)| {
            snapshot.status == 0 && snapshot.report["collection"]["ok"] == true
        });
        self.native = native;
        Some(result.map(|(mut snapshot, sources)| {
            if !current.same_launch_path(&job.request.key)
                && let Some(fields) = snapshot.report.as_object_mut()
            {
                fields.remove("board_checkout");
            }
            if self.accepted && job.request.native.is_none() {
                commands::publish_codex_quota_receipt(
                    &current.repo,
                    current.main.as_deref(),
                    &snapshot,
                    sources,
                );
            }
            snapshot
        }))
    }

    pub(crate) fn take_native_reading(&mut self) -> Option<crate::watch::NativeReading> {
        self.native.take()
    }

    pub(crate) fn stop_until(&mut self, deadline: Instant) {
        self.cancel();
        // Keep the CLI alive briefly so the producer can reap its tmux child.
        let _ = self
            .exited
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
    }

    /// Cancellation signals the producer and drops queued work without waiting for it.
    pub(crate) fn cancel(&mut self) {
        commands::clear_codex_quota_receipt();
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        self.pending.take();
        self.ready.take();
        self.native.take();
        self.idle.take();
        self.accepted = false;
    }
}

impl Drop for BoardRefresh {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn collect(
    job: &Job,
    service: &dyn Services,
    stop: &AtomicBool,
    receipts: &mut ReceiptTokensCache,
) -> Result<(Snapshot, CodexQuotaSources)> {
    let key = &job.request.key;
    require(key.current_repo(), "Board state repository changed")?;
    let (mut snapshot, sources, write_slots) = commands::snapshot_for(
        &job.session,
        &key.repo,
        key.room.as_deref(),
        key.main.as_deref(),
        &SnapshotReads { service, key },
        stop,
        true,
    )?;
    require(
        key.current_repo() && key.matches(&snapshot),
        "Board refresh scope changed",
    )?;
    // This view belongs to the interactive collector, not the public snapshot.
    snapshot.report["board_write_slots"] = write_slots.map_or(serde_json::Value::Null, |slots| {
        serde_json::json!({
            "used": slots.active + slots.awaiting + slots.unconfirmed,
            "limit": oms_panel_core::dispatch::effective_wip_limit(),
            "active": slots.active,
            "awaiting": slots.awaiting,
            "unconfirmed": slots.unconfirmed,
        })
    });
    if let (Some(attempt), Some(path)) = (&key.main, &key.launch_path)
        && snapshot.status == 0
        && snapshot.report["collection"]["ok"] == true
        && !stop.load(Ordering::Acquire)
        && let Some((worktree, branch)) = crate::native_children::observe_checkout(&key.repo, path)
    {
        snapshot.report["board_checkout"] = serde_json::json!({
            "attempt":attempt,"worktree":worktree,"branch":branch,"detached":branch.is_none()
        });
    }
    if key.room.is_some() && snapshot.status == 0 && snapshot.report["collection"]["ok"] == true {
        let mut attempts = snapshot.report["room_attempts"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(id, row)| (row["attempt_id"] == id.as_str()).then_some(id.as_str()))
            .peekable();
        if attempts.peek().is_some()
            && let Some(counts) = receipts.for_attempts(&key.repo, attempts, stop)
        {
            snapshot.report["board_receipt_tokens"] = counts.into();
        }
        require(key.current_repo(), "Board state repository changed")?;
    }
    Ok((snapshot, sources))
}

fn exact_room_args(repo: &Path, room: &str) -> Vec<OsString> {
    let mut args = commands::strings(&["show", "--id", room, "--exact", "--json", "--repo"]);
    args.push(repo.as_os_str().to_owned());
    args
}

struct BoardCollector {
    commands: SubcommandServices,
    events: EventsProjectionCache,
}

impl PeerExecutor for BoardCollector {
    fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
        Err(Error::new("Board refresh does not execute subprocesses"))
    }
}

impl Services for BoardCollector {
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        stop: &AtomicBool,
    ) -> Result<CommandOutput> {
        if command == NativeCommand::Events && args == ["list", "--json"] {
            let stdout = self.events.list_json(repo, stop).map_err(Error::new)?;
            return Ok(CommandOutput {
                stdout,
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            });
        }
        self.commands.command(command, repo, args, stop)
    }
}

struct SnapshotReads<'a> {
    service: &'a dyn Services,
    key: &'a RefreshKey,
}

impl PeerExecutor for SnapshotReads<'_> {
    fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
        Err(Error::new("Board refresh does not execute subprocesses"))
    }
}

impl Services for SnapshotReads<'_> {
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        stop: &AtomicBool,
    ) -> Result<CommandOutput> {
        require(repo == self.key.repo, "Board refresh repository mismatch")?;
        if command == NativeCommand::Events && args == ["list", "--json"] {
            return self.service.command(command, repo, args, stop);
        }
        if command == NativeCommand::Room
            && let Some(room) = &self.key.room
            && args == ["show", "--id", room, "--json"]
        {
            return self
                .service
                .command(command, repo, &exact_room_args(repo, room), stop);
        }
        Err(Error::new("command unavailable to Board refresh"))
    }
}

/// Short foreground reads used solely by the existing Codex chat custody proof.
/// A missing or stale attempt cache must never fall back to an Events replay.
pub(crate) struct ChatReads<'a>(&'a dyn Services);

impl<'a> ChatReads<'a> {
    pub(crate) fn new(service: &'a dyn Services) -> Self {
        Self(service)
    }
}

impl PeerExecutor for ChatReads<'_> {
    fn execute(&self, request: &ExecutionRequest, stop: &AtomicBool) -> Result<CommandOutput> {
        let args: Option<Vec<_>> = request.argv.iter().map(|word| word.to_str()).collect();
        let pane = |id| commands::valid_target(id, '%');
        let session = |id| commands::valid_target(id, '$');
        // The custody formats are plain substitutions. tmux format operators can execute commands.
        let format = |value: &str| {
            value.starts_with("#{session_id}")
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"#{}@_.\t".contains(&byte))
        };
        let read = match args.as_deref() {
            Some(["tmux", "list-panes", "-s", "-t", id, "-F", fields]) => {
                pane(id) && format(fields)
            }
            Some(["tmux", "display-message", "-p", "-t", id, fields]) => {
                (pane(id) || session(id)) && format(fields)
            }
            Some(
                [
                    "tmux",
                    "display-message",
                    "-p",
                    "-t",
                    identity,
                    PROVIDER_SESSION_IDENTITY_FORMAT,
                    ";",
                    "show-options",
                    "-v",
                    "-t",
                    owner,
                    "@oms_panel_repo",
                ],
            ) => session(identity) && identity == owner,
            Some(["tmux", "show-options", "-v", "-t", id, "@oms_panel_repo"]) => session(id),
            Some(["tmux", "capture-pane", "-p", "-J", "-S", "-100", "-t", id]) => pane(id),
            _ => false,
        };
        require(
            read && request.stdin.is_none()
                && request.environment.is_empty()
                && request.remove_environment.is_empty()
                && request.heartbeat.is_none(),
            "command unavailable to Board chat",
        )?;
        self.0.execute(request, stop)
    }
}

impl Services for ChatReads<'_> {
    fn command(
        &self,
        command: NativeCommand,
        repo: &Path,
        args: &[OsString],
        stop: &AtomicBool,
    ) -> Result<CommandOutput> {
        if command == NativeCommand::Room
            && let [verb, id, room, exact, json] = args
            && verb == "show"
            && id == "--id"
            && exact == "--exact"
            && json == "--json"
            && let Some(room) = room.to_str()
        {
            identifier(room)?;
            return self
                .0
                .command(command, repo, &exact_room_args(repo, room), stop);
        }
        Err(Error::new("command unavailable to Board chat"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oms_panel_core::Value;
    use serde_json::json;
    use std::os::unix::fs::DirBuilderExt;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(5);

    struct Probe {
        entered: Sender<(NativeCommand, Vec<OsString>)>,
        response: Receiver<Result<Value>>,
        stopped: Sender<()>,
    }

    impl PeerExecutor for Probe {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            panic!("worker executed a subprocess")
        }
    }

    impl Services for Probe {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            args: &[OsString],
            stop: &AtomicBool,
        ) -> Result<CommandOutput> {
            self.entered.send((command, args.to_owned())).unwrap();
            loop {
                if stop.load(Ordering::Acquire) {
                    self.stopped.send(()).unwrap();
                    return Err(Error::new("fixture read cancelled"));
                }
                match self.response.recv_timeout(Duration::from_millis(10)) {
                    Ok(result) => return result.map(output),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(Error::new("fixture disconnected"));
                    }
                }
            }
        }
    }

    fn output(value: Value) -> CommandOutput {
        CommandOutput {
            stdout: serde_json::to_vec(&value).unwrap(),
            cleanup_complete: true,
            output_complete: true,
            ..Default::default()
        }
    }

    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !condition() {
            assert!(Instant::now() < deadline, "refresh fixture timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn reply(worker: &mut BoardRefresh, key: &RefreshKey) -> Result<Snapshot> {
        let mut received = None;
        until(|| {
            received = worker.poll(key);
            received.is_some()
        });
        received.unwrap()
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn worker_coalesces_resets_scope_reuses_cache_and_cancels() {
        const CHILD: &str = "OMS_BOARD_REFRESH_TEST";
        if std::env::var_os(CHILD).is_none() {
            let home = Scratch(std::env::temp_dir().join(format!(
                "oms-board-refresh-{}",
                oms_command_plan::storage::random_hex(8).unwrap()
            )));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&home.0)
                .unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "board_refresh::tests::worker_coalesces_resets_scope_reuses_cache_and_cancels",
                ])
                .env_clear()
                .env("HOME", &home.0)
                .env("XDG_CACHE_HOME", home.0.join("cache"))
                .env("OMS_ROOM_ID", "room-ambient")
                .env("OMS_PANEL_WIP_LIMIT", "13")
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }

        let repo = PathBuf::from(std::env::var_os("HOME").unwrap())
            .canonicalize()
            .unwrap();
        let ledger = repo.join(".oms/lifecycle/events.jsonl");
        std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        std::fs::write(&ledger, b"generation").unwrap();
        let reservations = repo.join(".oms/artifacts/panel-dispatch/reservations.json");
        std::fs::create_dir_all(reservations.parent().unwrap()).unwrap();
        std::fs::write(
            &reservations,
            json!({"schema":1,"calls":{"call-orphan":{
                "room":"room-current","main_attempt":"attempt-current",
                "state":"reserved","route":{"access":"write"}
            }}})
            .to_string(),
        )
        .unwrap();
        let receipt_log = repo.join(".oms/usage/receipts.jsonl");
        std::fs::create_dir_all(receipt_log.parent().unwrap()).unwrap();
        let receipt = json!({
            "schema":2, "kind":"provider-usage", "call_id":"call-fixture",
            "operation_id":"op-fixture", "provider":"codex", "selected_model":null,
            "account":null, "goal_id":null, "task_id":null, "attempt_id":"attempt-current",
            "run_id":null, "ordinal":1, "revision":1, "phase":"final",
            "provider_usage":{"input_tokens":0,"output_tokens":0,"cache_read_tokens":0,
                "cache_write_tokens":null,"cache_in_input":true,"total_tokens":null},
            "cost":{"status":"unknown","basis":"unknown","microusd":null},
            "cumulative":{"session_id":"session-fixture","baseline_unknown":false},
            "source_sha256":null, "cleanup_complete":true
        });
        std::fs::write(&receipt_log, format!("{receipt}\n")).unwrap();
        let (entered, reads) = mpsc::channel();
        let (answer, response) = mpsc::channel();
        let (stopped, cancellation) = mpsc::channel();
        let mut worker = BoardRefresh::spawn_with(|| {
            Ok(Probe {
                entered,
                response,
                stopped,
            })
        })
        .unwrap();
        let old = RefreshKey::new(&repo, None, None).unwrap();
        let current =
            RefreshKey::new(&repo, Some("room-current"), Some("attempt-current")).unwrap();
        let list = || {
            assert_eq!(
                reads.recv_timeout(WAIT).unwrap(),
                (
                    NativeCommand::Events,
                    commands::strings(&["list", "--json"])
                )
            );
        };
        let room = || {
            assert_eq!(
                reads.recv_timeout(WAIT).unwrap(),
                (NativeCommand::Room, exact_room_args(&repo, "room-current"))
            );
        };
        let attempt = json!({
            "attempt_id":"attempt-current", "provider":"codex", "tool":"panel-main",
            "state":"working", "terminal":false, "refs":{"panel_role":"main","panel_room_id":"room-current"}
        });
        let room_state = json!({
            "id":"room-current", "closed":false,
            "participants":[{"participant":"attempt-current","provider":"codex","role":"main","joined":true}]
        });

        assert_eq!(worker.request(old.clone()).unwrap(), 1);
        list();
        for generation in 2..=50 {
            assert_eq!(worker.request(current.clone()).unwrap(), generation);
            assert!(worker.busy());
            assert!(worker.poll(&current).is_none());
            assert!(worker.idle_attempt_cache(&current).is_none());
        }
        assert!(reads.try_recv().is_err());
        answer.send(Err(Error::new("obsolete failure"))).unwrap();
        let mut launched = None;
        until(|| {
            assert!(worker.poll(&current).is_none(), "obsolete error published");
            launched = reads.try_recv().ok();
            launched.is_some()
        });
        assert_eq!(
            launched.unwrap(),
            (
                NativeCommand::Events,
                commands::strings(&["list", "--json"])
            )
        );
        answer.send(Ok(json!([attempt]))).unwrap();
        room();
        answer.send(Ok(room_state.clone())).unwrap();
        let accepted = reply(&mut worker, &current).unwrap();
        assert_eq!(accepted.status, 0);
        assert!(
            accepted.report["room_dispatches"]
                .get("call-orphan")
                .is_none()
        );
        assert_eq!(
            accepted.report["board_write_slots"],
            json!({"used":1,"limit":13,"active":0,"awaiting":0,"unconfirmed":1})
        );
        assert_eq!(
            accepted.report["board_receipt_tokens"]["attempt-current"]["total"],
            0
        );
        let calls = crate::render::calls(&accepted.report);
        assert_eq!(calls[0]["receipt_tokens"]["total"], 0);
        assert!(
            commands::take_codex_quota_receipt(
                &repo,
                "room-current",
                "attempt-current",
                &accepted.report
            )
            .is_some()
        );
        assert!(
            commands::take_codex_quota_receipt(
                &repo,
                "room-current",
                "attempt-current",
                &accepted.report.clone()
            )
            .is_none()
        );
        assert!(worker.idle_attempt_cache(&old).is_none());
        assert_eq!(
            worker
                .idle_attempt_cache(&current)
                .unwrap()
                .current_attempt(&repo, "attempt-current", &AtomicBool::new(false)),
            Some(attempt.clone())
        );

        let mut invalid_rooms = vec![json!({"id":"room-successor"})];
        for (field, value) in [
            ("closed", json!(true)),
            ("successor", json!("room-later")),
            (
                "participants",
                json!([
                    room_state["participants"][0],
                    {"participant":"foreign-main","provider":"codex","role":"main","joined":true}
                ]),
            ),
        ] {
            let mut invalid = room_state.clone();
            invalid[field] = value;
            invalid_rooms.push(invalid);
        }
        let mut missing_provider = room_state.clone();
        missing_provider["participants"][0]["provider"] = Value::Null;
        invalid_rooms.push(missing_provider);
        for invalid in invalid_rooms {
            worker.request(current.clone()).unwrap();
            room(); // Warm Session skips Events list even after a scope error.
            answer.send(Ok(invalid)).unwrap();
            assert_eq!(
                reply(&mut worker, &current).unwrap_err().message,
                "Board refresh scope changed"
            );
            assert!(worker.idle_attempt_cache(&current).is_none());
        }

        std::fs::write(&ledger, b"terminal generation").unwrap();
        worker.request(current.clone()).unwrap();
        list();
        let mut terminal = attempt.clone();
        terminal["terminal"] = json!(true);
        terminal["state"] = json!("done");
        answer.send(Ok(json!([terminal]))).unwrap();
        room();
        answer.send(Ok(room_state.clone())).unwrap();
        assert!(reply(&mut worker, &current).is_err());
        assert!(worker.idle_attempt_cache(&current).is_none());

        std::fs::write(&ledger, b"new live generation").unwrap();
        worker.request(current.clone()).unwrap();
        list();
        answer.send(Ok(json!([attempt]))).unwrap();
        room();
        answer.send(Ok(room_state.clone())).unwrap();
        assert_eq!(reply(&mut worker, &current).unwrap().status, 0);
        assert!(worker.idle_attempt_cache(&current).is_some());

        worker.request(current.clone()).unwrap();
        room();
        worker.request(current.clone()).unwrap();
        answer.send(Ok(room_state.clone())).unwrap();
        let mut launched = None;
        until(|| {
            assert!(
                worker.poll(&current).is_none(),
                "obsolete same-key result published"
            );
            launched = reads.try_recv().ok();
            launched.is_some()
        });
        assert_eq!(
            launched.unwrap(),
            (NativeCommand::Room, exact_room_args(&repo, "room-current"))
        );
        let mut latest_room = room_state;
        latest_room["generation"] = json!("latest request");
        std::fs::rename(&receipt_log, receipt_log.with_file_name("displaced.jsonl")).unwrap();
        std::fs::write(&receipt_log, b"{partial replacement").unwrap();
        answer.send(Ok(latest_room)).unwrap();
        let latest = reply(&mut worker, &current).unwrap().report;
        assert_eq!(latest["room"]["generation"], "latest request");
        assert!(latest.get("board_receipt_tokens").is_none());

        worker.request(old.clone()).unwrap();
        list(); // A Room/Main key change resets the whole Session.
        answer.send(Ok(json!([]))).unwrap();
        until(|| {
            assert!(
                worker.poll(&current).is_none(),
                "obsolete success published"
            );
            !worker.busy()
        });
        assert!(worker.idle_attempt_cache(&old).is_none());

        let replaceable = repo.join("replaceable");
        std::fs::create_dir(&replaceable).unwrap();
        let replaced = RefreshKey::new(&replaceable, None, None).unwrap();
        worker.request(replaced.clone()).unwrap();
        list();
        std::fs::rename(&replaceable, repo.join("displaced")).unwrap();
        std::fs::create_dir(&replaceable).unwrap();
        assert_ne!(replaced, RefreshKey::new(&replaceable, None, None).unwrap());
        answer.send(Ok(json!([]))).unwrap();
        until(|| {
            assert!(
                worker.poll(&replaced).is_none(),
                "replaced repo reply published"
            );
            !worker.busy()
        });

        worker.request(current.clone()).unwrap();
        list();
        worker.request(old.clone()).unwrap();
        worker.cancel();
        assert!(!worker.busy());
        assert!(worker.poll(&old).is_none());
        assert!(worker.request(old).is_err());
        cancellation.recv_timeout(WAIT).unwrap();
        assert!(reads.try_recv().is_err());
    }

    #[test]
    fn chat_reads_permit_exact_room_and_tmux_reads_but_never_events() {
        use std::cell::Cell;

        #[derive(Default)]
        struct Spy {
            calls: Cell<usize>,
        }
        impl PeerExecutor for Spy {
            fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                self.calls.set(self.calls.get() + 1);
                Ok(output(json!({})))
            }
        }
        impl Services for Spy {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                assert_eq!(command, NativeCommand::Room);
                assert_eq!(
                    args,
                    [
                        "show",
                        "--id",
                        "room-exact",
                        "--exact",
                        "--json",
                        "--repo",
                        repo.to_str().unwrap()
                    ]
                );
                self.calls.set(self.calls.get() + 1);
                Ok(output(json!({})))
            }
        }

        let spy = Spy::default();
        let chat = ChatReads::new(&spy);
        let repo = Path::new("/fixture");
        let stop = AtomicBool::new(false);
        let command = |domain, args| chat.command(domain, repo, &commands::strings(args), &stop);
        assert!(
            command(
                NativeCommand::Room,
                &["show", "--id", "room-exact", "--exact", "--json"]
            )
            .is_ok()
        );
        assert!(
            command(
                NativeCommand::Room,
                &["show", "--id", "room-exact", "--json"]
            )
            .is_err()
        );
        assert!(
            command(
                NativeCommand::Events,
                &["show", "--attempt", "attempt-current", "--json"]
            )
            .is_err()
        );
        assert!(command(NativeCommand::Events, &["list", "--json"]).is_err());
        let batch = [
            "tmux",
            "display-message",
            "-p",
            "-t",
            "$1",
            "#{session_id}\t#{pid}\t#{socket_path}\t.",
            ";",
            "show-options",
            "-v",
            "-t",
            "$1",
            "@oms_panel_repo",
        ];
        let allowed: &[&[&str]] = &[
            &batch,
            &[
                "tmux",
                "list-panes",
                "-s",
                "-t",
                "%1",
                "-F",
                crate::orchestrator::PANE_FORMAT,
            ],
            &[
                "tmux",
                "display-message",
                "-p",
                "-t",
                "$1",
                "#{session_id}\t#{pid}\t#{socket_path}",
            ],
            &["tmux", "show-options", "-v", "-t", "$1", "@oms_panel_repo"],
            &["tmux", "capture-pane", "-p", "-J", "-S", "-100", "-t", "%1"],
        ];
        for args in allowed {
            assert!(
                chat.execute(&ExecutionRequest::new(commands::strings(args), repo), &stop)
                    .is_ok()
            );
        }
        let denied: &[&[&str]] = &[
            &["tmux", "send-keys", "-t", "%1", "Enter"],
            &[
                "tmux",
                "display-message",
                "-p",
                "-t",
                "%1",
                "#{session_id}#(true)",
            ],
            &["tmux", "capture-pane", "-b", "buffer", "-t", "%1"],
        ];
        for args in denied {
            assert!(
                chat.execute(&ExecutionRequest::new(commands::strings(args), repo), &stop)
                    .is_err()
            );
        }
        for (index, value) in [
            (4, "%1"),
            (5, "#{session_id}\t#{pid}\t#{socket_path}"),
            (5, "#{session_id}#(true)\t."),
            (6, ";;"),
            (8, "-g"),
            (10, "$2"),
            (11, "@oms_panel_room"),
        ] {
            let mut near_miss = batch;
            near_miss[index] = value;
            assert!(
                chat.execute(
                    &ExecutionRequest::new(commands::strings(&near_miss), repo),
                    &stop
                )
                .is_err()
            );
        }
        let mut extra = batch.to_vec();
        extra.extend([";", "show-options", "-v", "-t", "$1", "@oms_panel_repo"]);
        assert!(
            chat.execute(
                &ExecutionRequest::new(commands::strings(&extra), repo),
                &stop
            )
            .is_err()
        );
        assert_eq!(spy.calls.get(), 6);
    }
}
