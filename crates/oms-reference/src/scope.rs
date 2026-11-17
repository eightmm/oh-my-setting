//! Immutable private return channels and fenced heartbeat publication.
//! Shell helper actions and the in-process owner use the same disk protocol.
use crate::{
    Result, V, checkpoint,
    controller::Controller,
    digest, encoded, hex, obj,
    process::{IdentitySource, Owner, OwnerError},
    publication, storage, text,
};
use oms_command_operations as events;
use std::{
    fs::{self, File},
    io::Write,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub type Validator<'a> = dyn FnMut(&[V], Option<&V>) -> Result<()> + 'a;
pub type CheckedAppend = fn(&Path, V, &AtomicBool, &mut Validator<'_>) -> Result<V>;

/// Uses the operations producer's own lock and fresh projection. The callback
/// executes before idempotent reuse or append and must not reacquire that lock.
pub fn native_checked_append(
    repo: &Path,
    candidate: V,
    interrupted: &AtomicBool,
    check: &mut Validator<'_>,
) -> Result<V> {
    events::append_lifecycle_checked(repo, candidate, interrupted, check)
}

#[derive(Clone)]
pub struct Protocol {
    helper: String,
    identity: Arc<dyn IdentitySource>,
    checked: CheckedAppend,
    controller: Option<Controller>,
}
impl Protocol {
    /// Explicit service injection: the trusted caller owns authentication and
    /// freshness of this helper identity. A hex string alone is not a native
    /// proof. Production callers use `production` or `production_with_controller`.
    pub fn new(
        helper_sha256: String,
        identity: Arc<dyn IdentitySource>,
        checked: CheckedAppend,
    ) -> Result<Self> {
        if !hex(&helper_sha256, 64) {
            return Err("invalid native reference helper digest".into());
        }
        Ok(Self {
            helper: helper_sha256,
            identity,
            checked,
            controller: None,
        })
    }
    /// Retains one native executing-image snapshot with injected process and
    /// lifecycle services. This never hashes a named current_exe replacement.
    pub fn for_executable(
        identity: Arc<dyn IdentitySource>,
        checked: CheckedAppend,
    ) -> Result<Self> {
        Self::with_controller(Controller::capture_current()?, identity, checked)
    }
    pub fn native(checked: CheckedAppend) -> Result<Self> {
        Self::for_executable(Arc::new(crate::process::NativeIdentity), checked)
    }
    pub fn with_controller(
        controller: Controller,
        identity: Arc<dyn IdentitySource>,
        checked: CheckedAppend,
    ) -> Result<Self> {
        let helper = controller.digest()?.to_owned();
        Ok(Self {
            helper,
            identity,
            checked,
            controller: Some(controller),
        })
    }
    pub fn production() -> Result<Self> {
        Self::production_with_controller(Controller::capture_current()?)
    }
    pub fn production_with_controller(controller: Controller) -> Result<Self> {
        Self::with_controller(
            controller,
            Arc::new(crate::process::NativeIdentity),
            native_checked_append,
        )
    }
    pub fn controller(&self) -> Option<&Controller> {
        self.controller.as_ref()
    }
    fn check_controller(&self) -> Result<()> {
        if let Some(controller) = &self.controller {
            controller.check()?;
        }
        Ok(())
    }
    pub fn read(&self, repo: &Path, channel: &Path, expected: &str) -> Result<(V, fs::Metadata)> {
        self.check_controller()?;
        let (raw, info) = storage::regular(channel, 4096, 1)?;
        if digest(&raw) != expected {
            return Err("reference scope changed".into());
        }
        let scope = V::parse(&raw)?;
        let binding = scope.get("binding");
        let physical = fs::canonicalize(repo).map_err(|e| e.to_string())?;
        if !matches!(&scope,V::Obj(v) if v.len() == 2 && v.contains_key(b"binding".as_slice()) && v.contains_key(b"attempt".as_slice()))
            || !matches!(binding, V::Obj(_))
            || binding.get("repo").text() != Some(&digest(physical.as_os_str().as_bytes()))
            || binding.get("helper").text() != Some(&self.helper)
        {
            return Err("foreign reference scope".into());
        }
        let directory = storage::private_directory(channel.parent().ok_or("scope has no parent")?)?;
        if storage::identity(&info) != *binding.get("channel")
            || storage::identity(&directory) != *binding.get("directory")
            || fs::canonicalize(channel).map_err(|e| e.to_string())? != storage::absolute(channel)?
        {
            return Err("reference scope directory or file replaced".into());
        }
        Owner::parse(binding.get("owner"))?;
        text(&scope, "attempt")?;
        self.check_controller()?;
        Ok((scope, info))
    }
    pub fn begin_owned(
        &self,
        repo: &Path,
        tool: &str,
        input: &Path,
        channel: &Path,
        output: &mut dyn Write,
        interrupted: &AtomicBool,
    ) -> Result<(String, String)> {
        let owner = self
            .identity
            .foreground_parent()
            .map_err(|e| e.to_string())?;
        self.register(
            repo,
            tool,
            input,
            channel,
            owner,
            "foreground-parent-v1",
            output,
            interrupted,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn register(
        &self,
        repo: &Path,
        tool: &str,
        input: &Path,
        channel: &Path,
        owner: Owner,
        owner_source: &str,
        output: &mut dyn Write,
        interrupted: &AtomicBool,
    ) -> Result<(String, String)> {
        checkpoint(interrupted)?;
        self.check_controller()?;
        let (raw, info) = storage::regular(channel, 0, 1)?;
        let root = channel.parent().ok_or("scope has no parent")?;
        let directory = storage::private_directory(root)?;
        if !raw.is_empty()
            || fs::canonicalize(channel).map_err(|e| e.to_string())? != storage::absolute(channel)?
        {
            return Err("unsafe reference return channel".into());
        }
        let pending_path = auxiliary(channel, ".ready.pending");
        let pending = storage::create_private(
            root,
            Path::new(
                pending_path
                    .file_name()
                    .ok_or("pending fence has no name")?,
            ),
            b"",
        )?;
        let physical = fs::canonicalize(repo).map_err(|e| e.to_string())?;
        let binding = obj([
            ("owner", owner.value()),
            ("owner_source", owner_source.into()),
            ("nonce", storage::random_hex(32)?.into()),
            ("tool", tool.into()),
            (
                "input",
                digest(storage::absolute(input)?.as_os_str().as_bytes()).into(),
            ),
            ("repo", digest(physical.as_os_str().as_bytes()).into()),
            ("channel", storage::identity(&info)),
            ("ready_pending", storage::identity(&pending)),
            ("directory", storage::identity(&directory)),
            ("helper", self.helper.clone().into()),
        ]);
        let mut request = publication::request(tool, Some(input))?;
        request
            .refs
            .set("reference_scope_sha256", digest(&encoded(&binding)?));
        let attempt = events::create_attempt(&physical, &request, interrupted)?;
        publication::transition(&physical, &attempt, "starting", interrupted)?;
        publication::transition(&physical, &attempt, "working", interrupted)?;
        self.require_owner(&owner)?;
        self.check_controller()?;
        let payload = encoded(&obj([
            ("binding", binding),
            ("attempt", attempt.clone().into()),
        ]))?;
        write_empty(channel, &info, &payload)?;
        let expected = digest(&payload);
        writeln!(output, "{attempt} {expected}")
            .and_then(|()| output.flush())
            .map_err(|e| e.to_string())?;
        Ok((attempt, expected))
    }
    fn require_owner(&self, owner: &Owner) -> Result<()> {
        if self
            .identity
            .owner_identity(owner.pid)
            .map_err(|e| e.to_string())?
            != *owner
        {
            return Err("reference owner generation changed".into());
        }
        Ok(())
    }
    fn caller(&self, binding: &V, in_process: bool) -> Result<()> {
        let source = if in_process {
            "native-inprocess-v1"
        } else {
            "foreground-parent-v1"
        };
        if binding.get("owner_source").text() != Some(source) {
            return Err("reference owner source differs from this protocol action".into());
        }
        let expected = Owner::parse(binding.get("owner"))?;
        let observed = if in_process {
            self.identity.owner_identity(std::process::id())
        } else {
            self.identity.foreground_parent()
        }
        .map_err(|e| e.to_string())?;
        if observed != expected {
            return Err("reference scope belongs to another writer".into());
        }
        Ok(())
    }
    pub fn pulse(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        interrupted: &AtomicBool,
    ) -> Result<bool> {
        let (initial, _) = self.read(repo, channel, expected)?;
        let attempt = text(&initial, "attempt")?;
        let event = events::new_event(
            attempt,
            0usize.into(),
            "attempt.heartbeat",
            obj([(
                "actor",
                obj([
                    ("kind", "runner".into()),
                    ("name", "landing-reference".into()),
                ]),
            )]),
        )?;
        let mut ended = false;
        let mut validate = |rows: &[V], current: Option<&V>| {
            let (scope, _) = self.read(repo, channel, expected)?;
            let binding = scope.get("binding");
            self.require_owner(&Owner::parse(binding.get("owner"))?)?;
            let current = current.ok_or("reference scope is not the registered writer")?;
            let created = rows
                .iter()
                .filter(|r| {
                    r.get("attempt_id").text() == Some(attempt)
                        && r.get("event_type").text() == Some("attempt.created")
                })
                .collect::<Vec<_>>();
            if created.len() != 1
                || current.get("provider").text() != Some("local")
                || current.get("tool") != binding.get("tool")
                || current.get("parent_attempt_id").truth()
                || created[0].get("refs").get("reference_writer").text()
                    != Some("landing-reference-v1")
                || created[0].get("refs").get("reference_input_sha256") != binding.get("input")
                || created[0].get("refs").get("reference_scope_sha256").text()
                    != Some(&digest(&encoded(binding)?))
            {
                return Err("reference scope is not the registered writer".into());
            }
            if !matches!(
                current.get("state").text(),
                Some("starting" | "working" | "verifying")
            ) {
                ended = true;
                return Err("reference operation ended".into());
            }
            Ok(())
        };
        let result = bounded(repo, interrupted, |flag, acquired| {
            let mut checked = |rows: &[V], current: Option<&V>| {
                acquired.store(true, Ordering::Relaxed);
                validate(rows, current)
            };
            (self.checked)(repo, event, flag, &mut checked)
        });
        match result {
            Ok(_) => Ok(true),
            Err(_) if ended => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub fn monitor(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        self.monitor_owned(repo, channel, expected, interrupted, false)
    }
    fn monitor_owned(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        interrupted: &AtomicBool,
        in_process: bool,
    ) -> Result<()> {
        let (initial, _) = self.read(repo, channel, expected)?;
        self.caller(initial.get("binding"), in_process)
            .map_err(|_| "reference monitor belongs to another writer")?;
        if !self.pulse(repo, channel, expected, interrupted)? {
            return Err("reference operation ended before monitor startup".into());
        }
        let (scope, _) = self.read(repo, channel, expected)?;
        let pending = auxiliary(channel, ".ready.pending");
        let (raw, info) = storage::regular(&pending, 0, 1)?;
        if !raw.is_empty() || storage::identity(&info) != *scope.get("binding").get("ready_pending")
        {
            return Err("reference publication fence changed".into());
        }
        let ready = encoded(&obj([
            ("scope", expected.into()),
            (
                "monitor",
                self.identity
                    .owner_identity(std::process::id())
                    .map_err(|e| e.to_string())?
                    .value(),
            ),
        ]))?;
        write_empty(&pending, &info, &ready)?;
        self.read(repo, channel, expected)?;
        let (_, written) = storage::regular(&pending, 512, 1)?;
        let root = channel.parent().ok_or("scope has no parent")?;
        let ready_path = auxiliary(channel, ".ready");
        storage::link(
            &pending,
            root,
            Path::new(ready_path.file_name().ok_or("readiness has no name")?),
            &written,
        )?;
        let named = fs::symlink_metadata(&pending).map_err(|e| e.to_string())?;
        if storage::identity(&named) != *scope.get("binding").get("ready_pending") {
            return Err("reference publication fence replaced before release".into());
        }
        fs::remove_file(&pending).map_err(|e| e.to_string())?;
        storage::sync_directory(root)?;
        let mut next_pulse = Instant::now() + Duration::from_secs(10);
        let mut next_owner = Instant::now() + Duration::from_secs(1);
        loop {
            checkpoint(interrupted)?;
            let current = Instant::now();
            if in_process && current < next_owner && current < next_pulse {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let (scope, _) = match self.read(repo, channel, expected) {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
            if current >= next_owner {
                self.require_owner(&Owner::parse(scope.get("binding").get("owner"))?)?;
                let state = bounded(repo, interrupted, |flag, acquired| {
                    let _guard = events::lock::FileLock::acquire(&events::event_path(repo), flag)?;
                    acquired.store(true, Ordering::Relaxed);
                    self.read(repo, channel, expected)?;
                    let (_, attempts) = events::load_projection(repo)?;
                    Ok(attempts
                        .iter()
                        .find(|v| v.get("attempt_id") == scope.get("attempt"))
                        .is_some_and(|v| {
                            matches!(
                                v.get("state").text(),
                                Some("starting" | "working" | "verifying")
                            )
                        }))
                });
                match state {
                    Ok(false) => return Ok(()),
                    Ok(true) => {}
                    Err(error) if contention(&error) => {}
                    Err(error) => return Err(error),
                }
                next_owner = current + Duration::from_secs(1);
            }
            if current >= next_pulse {
                match self.pulse(repo, channel, expected, interrupted) {
                    Ok(false) => return Ok(()),
                    Ok(true) => next_pulse = Instant::now() + Duration::from_secs(10),
                    Err(error) if contention(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn await_monitor(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        native_monitor: &str,
        interrupted: &AtomicBool,
    ) -> Result<String> {
        self.await_owned(repo, channel, expected, native_monitor, interrupted, false)
    }
    fn await_owned(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        native_monitor: &str,
        interrupted: &AtomicBool,
        in_process: bool,
    ) -> Result<String> {
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            checkpoint(interrupted)?;
            let (scope, _) = self.read(repo, channel, expected)?;
            self.caller(scope.get("binding"), in_process)
                .map_err(|_| "reference readiness belongs to another writer")?;
            let pending = auxiliary(channel, ".ready.pending");
            let ready = auxiliary(channel, ".ready");
            if crate::capture::exists(&pending)? {
                let info = fs::symlink_metadata(&pending).map_err(|e| e.to_string())?;
                if !info.is_file()
                    || !matches!(info.nlink(), 1 | 2)
                    || info.size() > 512
                    || info.uid() != rustix::process::getuid().as_raw()
                    || storage::identity(&info) != *scope.get("binding").get("ready_pending")
                {
                    return Err("reference publication fence changed while waiting".into());
                }
                if crate::capture::exists(&ready)?
                    && storage::identity(&fs::symlink_metadata(&ready).map_err(|e| e.to_string())?)
                        != *scope.get("binding").get("ready_pending")
                {
                    return Err("foreign reference readiness during publication".into());
                }
            } else if crate::capture::exists(&ready)? {
                let (raw, _) = storage::regular(&ready, 512, 1)?;
                if !raw.is_empty() {
                    let value = V::parse(&raw)?;
                    if !matches!(&value,V::Obj(v) if v.len()==2)
                        || value.get("scope").text() != Some(expected)
                        || value.get("monitor").get("pid").py() != native_monitor
                    {
                        return Err("reference monitor readiness changed".into());
                    }
                    let owner = Owner::parse(value.get("monitor"))?;
                    self.require_owner(&owner)?;
                    self.caller(scope.get("binding"), in_process)
                        .map_err(|_| "reference readiness belongs to another writer")?;
                    return Ok(digest(&raw));
                }
            }
            if Instant::now() >= deadline {
                return Err("reference monitor did not become ready".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn close_scope(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        interrupted: &AtomicBool,
    ) -> Result<()> {
        self.close_owned(repo, channel, expected, interrupted, false)
    }
    fn close_owned(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        interrupted: &AtomicBool,
        in_process: bool,
    ) -> Result<()> {
        bounded(repo, interrupted, |flag, acquired| {
            let _guard = events::lock::FileLock::acquire(&events::event_path(repo), flag)?;
            acquired.store(true, Ordering::Relaxed);
            let (scope, info) = self.read(repo, channel, expected)?;
            self.caller(scope.get("binding"), in_process)?;
            if !storage::same(
                &info,
                &fs::symlink_metadata(channel).map_err(|e| e.to_string())?,
            ) {
                return Err("reference scope replaced before close".into());
            }
            fs::remove_file(channel).map_err(|e| e.to_string())?;
            let root = channel.parent().ok_or("scope has no parent")?;
            storage::sync_directory(root)?;
            let mut auxiliary_files = Vec::new();
            let mut ready_raw = None;
            let ready = auxiliary(channel, ".ready");
            if crate::capture::exists(&ready)? {
                let (raw, info) = storage::regular(&ready, 512, 1)?;
                let value = V::parse(&raw)?;
                if !matches!(&value,V::Obj(v) if v.len()==2)
                    || value.get("scope").text() != Some(expected)
                {
                    return Err("reference readiness changed before close".into());
                }
                ready_raw = Some(raw);
                auxiliary_files.push((ready, info));
            }
            let result_path = root.join("result");
            let (result, result_info) = storage::regular(&result_path, 256, 1)?;
            if result != format!("{} {expected}\n", text(&scope, "attempt")?).as_bytes() {
                return Err("reference return file changed before close".into());
            }
            auxiliary_files.push((result_path, result_info));
            let proof_path = root.join("proof");
            let (proof, proof_info) = storage::regular(&proof_path, 66, 1)?;
            let expected_proof = ready_raw
                .map(|v| format!("{}\n", digest(&v)).into_bytes())
                .unwrap_or_default();
            if proof != expected_proof {
                return Err("reference readiness proof changed before close".into());
            }
            auxiliary_files.push((proof_path, proof_info));
            for (name, before) in auxiliary_files {
                if !storage::same(
                    &before,
                    &fs::symlink_metadata(&name).map_err(|e| e.to_string())?,
                ) {
                    return Err("reference auxiliary replaced before cleanup".into());
                }
                fs::remove_file(name).map_err(|e| e.to_string())?;
            }
            storage::sync_directory(root)?;
            fs::remove_dir(root).map_err(|e| e.to_string())?;
            storage::sync_directory(root.parent().ok_or("scope directory has no parent")?)
        })
    }
    pub fn stop_monitor(
        &self,
        repo: &Path,
        channel: &Path,
        expected: &str,
        ready_digest: &str,
        stderr: &mut dyn Write,
        interrupted: &AtomicBool,
    ) -> Result<i32> {
        let (scope, _) = self.read(repo, channel, expected)?;
        self.caller(scope.get("binding"), false)
            .map_err(|_| "reference stop belongs to another writer")?;
        let ready = (|| {
            let (raw, _) = storage::regular(&auxiliary(channel, ".ready"), 512, 1)?;
            let value = V::parse(&raw)?;
            if !matches!(&value,V::Obj(v) if v.len()==2)
                || value.get("scope").text() != Some(expected)
                || ready_digest.is_empty()
                || digest(&raw) != ready_digest
            {
                return Err("invalid readiness".to_string());
            }
            Owner::parse(value.get("monitor"))
        })()
        .ok();
        let cleanup_failed = match self.close_scope(repo, channel, expected, interrupted) {
            Ok(()) => false,
            Err(error) => {
                writeln!(stderr, "landing-reference: scope preserved: {error}")
                    .map_err(|e| e.to_string())?;
                true
            }
        };
        let identity = ready
            .ok_or("reference monitor identity unavailable; scope closed without signalling")?;
        let mut deadline = Instant::now() + Duration::from_millis(1500);
        let mut forced = false;
        loop {
            checkpoint(interrupted)?;
            match self.identity.owner_identity(identity.pid) {
                Err(OwnerError::Exited) => return Ok(if cleanup_failed { 3 } else { 0 }),
                Ok(current) if current != identity => {
                    return Ok(if cleanup_failed { 3 } else { 0 });
                }
                Err(error) => return Err(error.to_string()),
                Ok(_) => {}
            }
            if Instant::now() >= deadline {
                if forced || !crate::process::terminate_monitor(self.identity.as_ref(), &identity)?
                {
                    return Err(
                        "reference monitor stop unproven; not waiting on an unknown child".into(),
                    );
                }
                forced = true;
                deadline = Instant::now() + Duration::from_millis(300);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
fn auxiliary(channel: &Path, suffix: &str) -> PathBuf {
    let mut path = channel.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}
fn write_empty(path: &Path, expected: &fs::Metadata, bytes: &[u8]) -> Result<()> {
    use rustix::fs::{self as rfs, Mode, OFlags};
    let root = path.parent().ok_or("scope file has no parent")?;
    storage::private_directory(root)?;
    let snapshot = storage::Namespace::snapshot(root, path)?;
    let mut file: File = rfs::open(
        path,
        OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?
    .into();
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file()
        || opened.nlink() != 1
        || opened.size() != 0
        || !storage::generation(expected, &opened)
    {
        return Err("reference return channel replaced".into());
    }
    snapshot.check()?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    snapshot.check()?;
    if !storage::generation(
        &file.metadata().map_err(|e| e.to_string())?,
        &fs::symlink_metadata(path).map_err(|e| e.to_string())?,
    ) {
        return Err("reference channel replaced during publication".into());
    }
    storage::sync_directory(root)
}
fn contention(message: &str) -> bool {
    message.starts_with("could not acquire state lock for ")
}
/// Bound the legacy one-second lock attempts without changing process-global
/// environment or imposing a timer on normal command output.
fn bounded<T>(
    repo: &Path,
    interrupted: &AtomicBool,
    work: impl FnOnce(&AtomicBool, &AtomicBool) -> Result<T>,
) -> Result<T> {
    let cancelled = AtomicBool::new(false);
    let expired = AtomicBool::new(false);
    let acquired = AtomicBool::new(false);
    let (done, wait) = std::sync::mpsc::channel();
    let result = std::thread::scope(|scope| {
        let cancelled = &cancelled;
        let expired = &expired;
        let acquired = &acquired;
        scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if interrupted.load(Ordering::Relaxed) {
                    cancelled.store(true, Ordering::Relaxed);
                    return;
                }
                if !acquired.load(Ordering::Relaxed) && Instant::now() >= deadline {
                    expired.store(true, Ordering::Relaxed);
                    cancelled.store(true, Ordering::Relaxed);
                    return;
                }
                match wait.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        let result = work(cancelled, acquired);
        let _ = done.send(());
        result
    });
    if result.is_err() && expired.load(Ordering::Relaxed) && !interrupted.load(Ordering::Relaxed) {
        Err(format!(
            "could not acquire state lock for {}",
            events::event_path(repo).display()
        ))
    } else {
        result
    }
}

/// An owned OS thread cannot survive its process. Its retained JoinHandle and
/// cancellation flag fence cleanup before return; no PID signal is involved.
/// Disk scope/readiness and canonical lifecycle heartbeats remain observable
/// to cooperating helpers and GC throughout the actual foreground operation.
pub struct OwnedReference {
    protocol: Protocol,
    repo: PathBuf,
    target: PathBuf,
    channel: PathBuf,
    attempt: String,
    expected: String,
    interrupted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    monitor: Option<std::thread::JoinHandle<()>>,
    monitor_error: Arc<std::sync::Mutex<Option<String>>>,
    index: Option<crate::capture::CaptureIndex>,
    allowed_write: bool,
    closed: bool,
}
impl OwnedReference {
    pub fn begin(
        protocol: Protocol,
        repo: &Path,
        target: &Path,
        interrupted: Arc<AtomicBool>,
    ) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        checkpoint(&interrupted)?;
        let owner = protocol
            .identity
            .owner_identity(std::process::id())
            .map_err(|e| e.to_string())?;
        let parent = fs::canonicalize(std::env::temp_dir()).map_err(|e| e.to_string())?;
        let root = parent.join(format!("oms-reference.{}", storage::random_hex(16)?));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|e| e.to_string())?;
        storage::sync_directory(&parent)?;
        let channel = root.join("scope");
        storage::create_private(&root, Path::new("scope"), b"")?;
        let result_info = storage::create_private(&root, Path::new("result"), b"")?;
        storage::create_private(&root, Path::new("proof"), b"")?;
        let repo = fs::canonicalize(repo).map_err(|e| e.to_string())?;
        let target = storage::absolute(target)?;
        let mut result = Vec::new();
        let (attempt, expected) = protocol.register(
            &repo,
            "agent-task",
            &target,
            &channel,
            owner,
            "native-inprocess-v1",
            &mut result,
            &interrupted,
        )?;
        write_empty(&root.join("result"), &result_info, &result)?;
        let mut owned = Self {
            protocol,
            repo,
            target,
            channel,
            attempt,
            expected,
            interrupted,
            stop: Arc::new(AtomicBool::new(false)),
            monitor: None,
            monitor_error: Arc::new(std::sync::Mutex::new(None)),
            index: None,
            allowed_write: false,
            closed: false,
        };
        let protocol = owned.protocol.clone();
        let repo = owned.repo.clone();
        let channel = owned.channel.clone();
        let expected = owned.expected.clone();
        let stop = owned.stop.clone();
        let errors = owned.monitor_error.clone();
        owned.monitor = Some(
            std::thread::Builder::new()
                .name("oms-reference-monitor".into())
                .spawn(move || {
                    let outcome = protocol.monitor_owned(&repo, &channel, &expected, &stop, true);
                    if let Err(error) = outcome
                        && !stop.load(Ordering::Relaxed)
                    {
                        match errors.lock() {
                            Ok(mut errors) => *errors = Some(error),
                            Err(poisoned) => {
                                *poisoned.into_inner() =
                                    Some("reference monitor failure state is poisoned".into())
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        let proof = owned.protocol.await_owned(
            &owned.repo,
            &owned.channel,
            &owned.expected,
            &std::process::id().to_string(),
            &owned.interrupted,
            true,
        )?;
        let (_, info) = storage::regular(&root.join("proof"), 0, 1)?;
        write_empty(&root.join("proof"), &info, format!("{proof}\n").as_bytes())?;
        owned.healthy()?;
        Ok(owned)
    }
    pub fn attempt(&self) -> &str {
        &self.attempt
    }
    fn healthy(&self) -> Result<()> {
        checkpoint(&self.interrupted)?;
        let error = self
            .monitor_error
            .lock()
            .map_err(|_| "reference monitor failure state is poisoned")?;
        if let Some(error) = error.as_ref() {
            return Err(error.clone());
        }
        let (scope, _) = self
            .protocol
            .read(&self.repo, &self.channel, &self.expected)?;
        self.protocol.caller(scope.get("binding"), true)?;
        Ok(())
    }
    pub fn validate(
        &mut self,
        request: &oms_command_task::services::ReferenceWrite<'_>,
    ) -> Result<()> {
        self.healthy()?;
        if fs::canonicalize(request.repo).map_err(|e| e.to_string())? != self.repo {
            return Err("reference operation belongs to another state repository".into());
        }
        let target = storage::absolute(request.target)?;
        // Archive is a same-operation destination; original before/proposed
        // bytes are still validated against its actual scanned namespace.
        if target != self.target && request.operation != "agent_task_archive_unlocked" {
            return Err("reference operation belongs to another task packet".into());
        }
        if !self
            .protocol
            .pulse(&self.repo, &self.channel, &self.expected, &self.interrupted)?
        {
            return Err("reference operation is no longer active".into());
        }
        let result = (|| {
            if self.index.is_none() {
                self.index = Some(crate::capture::CaptureIndex::load(&self.repo)?);
            }
            self.index
                .as_ref()
                .ok_or("reference capture snapshot unavailable")?
                .validate_write(
                    &target,
                    request.operation,
                    request.arguments,
                    request.before,
                    request.proposed,
                    request.source_session,
                )
        })();
        if let Err(error) = result {
            // Only the first refusal is proof that this operation has not
            // allowed any publication. Later uncertainty remains nonterminal.
            if !self.allowed_write {
                publication::finish(
                    &self.repo,
                    &self.attempt,
                    "refuse-no-write",
                    &self.interrupted,
                )?;
            }
            return Err(error);
        }
        self.allowed_write |= request.before != request.proposed || target != self.target;
        Ok(())
    }
    pub fn finish(&mut self, exit: i32) -> Result<()> {
        let result = (|| {
            if exit == 0 {
                self.healthy()?;
            }
            publication::finish(
                &self.repo,
                &self.attempt,
                &exit.to_string(),
                &self.interrupted,
            )
        })();
        let stopped = self.close();
        result.and(stopped)
    }
    fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(monitor) = self.monitor.take() {
            monitor
                .join()
                .map_err(|_| "reference monitor panicked; operation remains unresolved")?;
        }
        // Cleanup cannot be skipped merely because the caller was interrupted.
        // Its own bounded lifecycle wait is independent of user cancellation.
        let result = self.protocol.close_owned(
            &self.repo,
            &self.channel,
            &self.expected,
            &AtomicBool::new(false),
            true,
        );
        self.closed = true;
        result
    }
}
impl Drop for OwnedReference {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
impl oms_command_task::services::ReferenceOperation for OwnedReference {
    fn validate(
        &mut self,
        request: &oms_command_task::services::ReferenceWrite<'_>,
    ) -> oms_command_task::Result<()> {
        OwnedReference::validate(self, request)
            .map_err(|error| oms_command_task::Error::new(1, error))
    }
    fn finish(&mut self, exit: i32) -> oms_command_task::Result<()> {
        OwnedReference::finish(self, exit).map_err(|error| oms_command_task::Error::new(1, error))
    }
}
/// Explicit adapter into the existing task trait; no reverse task dependency
/// or replacement lifecycle writer is required.
pub fn task_operation(
    protocol: Protocol,
    repo: &Path,
    target: &Path,
    interrupted: Arc<AtomicBool>,
) -> oms_command_task::Result<Box<dyn oms_command_task::services::ReferenceOperation>> {
    OwnedReference::begin(protocol, repo, target, interrupted)
        .map(|operation| {
            Box::new(operation) as Box<dyn oms_command_task::services::ReferenceOperation>
        })
        .map_err(|error| oms_command_task::Error::new(1, error))
}

pub fn native_task_operation(
    repo: &Path,
    target: &Path,
    interrupted: Arc<AtomicBool>,
) -> oms_command_task::Result<Box<dyn oms_command_task::services::ReferenceOperation>> {
    let protocol =
        Protocol::production().map_err(|error| oms_command_task::Error::new(1, error))?;
    task_operation(protocol, repo, target, interrupted)
}

pub fn task_operation_with_controller(
    controller: Controller,
    repo: &Path,
    target: &Path,
    interrupted: Arc<AtomicBool>,
) -> oms_command_task::Result<Box<dyn oms_command_task::services::ReferenceOperation>> {
    let protocol = Protocol::production_with_controller(controller)
        .map_err(|error| oms_command_task::Error::new(1, error))?;
    task_operation(protocol, repo, target, interrupted)
}
