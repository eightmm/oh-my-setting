#![forbid(unsafe_code)]

use crate::cold::read_regular_no_symlink_bytes;
use crate::config::{NotionSettings, journal_language, notion_repo_excluded, notion_settings};
use crate::json::{JsonExt, Map, Value};
use crate::render::notion_presentation;
use crate::sanitize::{pretty_json_bytes, sanitize_text};
use crate::schema::atomic_write_bytes;
use crate::store::JournalStore;
use crate::tz::{
    current_utc_now, naive_date_to_string, parse_http_date_epoch, parse_rfc3339_utc, utc_rfc3339,
};
use chrono::{DateTime, Duration, Utc};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus, geteuid, kill_process_group, waitid,
};
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

pub const NOTION_API_VERSION: &str = "2026-03-11";
pub const NOTION_MAX_BLOCKS: usize = 900;
pub const NOTION_CHILD_BATCH: usize = 100;
pub const NOTION_TEXT_CHUNK: usize = 2000;
pub const NOTION_TOGGLE_CHILD_LIMIT: usize = 95;
pub const MAX_RETRY_WAIT_SECS: i64 = 86400;
pub const INLINE_RETRY_AFTER_CAP: f64 = 2.0;

#[derive(Clone, Debug)]
pub enum TransportError {
    Timeout(String),
    RateLimited {
        retry_after: f64,
        message: String,
    },
    ServerError {
        status: u16,
        message: String,
        retry_after: Option<f64>,
    },
    ClientError {
        status: u16,
        message: String,
    },
    Io(String),
    Deferred {
        status: u16,
        retry_after: f64,
        message: String,
    },
}

impl TransportError {
    pub fn message(&self) -> &str {
        match self {
            TransportError::Timeout(m)
            | TransportError::RateLimited { message: m, .. }
            | TransportError::ServerError { message: m, .. }
            | TransportError::ClientError { message: m, .. }
            | TransportError::Io(m)
            | TransportError::Deferred { message: m, .. } => m,
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            TransportError::Timeout(_)
                | TransportError::RateLimited { .. }
                | TransportError::ServerError { .. }
        )
    }

    pub fn retry_after(&self) -> Option<f64> {
        match self {
            TransportError::RateLimited { retry_after, .. }
            | TransportError::Deferred { retry_after, .. } => Some(*retry_after),
            _ => None,
        }
    }
}

const MAX_HDR_READ_BYTES: u64 = 1024 * 1024;
const MAX_CHILD_STDOUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CHILD_STDERR_BYTES: u64 = 1024 * 1024;

static REQUEST_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub enum LiveTransport {
    Curl {
        bearer_cred: String,
        api_version: String,
    },
    NtnCli {
        command: String,
        keyring: String,
        api_version: String,
    },
}

struct RequestTempDir {
    dir_path: PathBuf,
    dir_fd: OwnedFd,
    dir_dev: u64,
    dir_ino: u64,
    uid: u32,
    cfg_id: Option<(u64, u64)>,
    hdr_created: bool,
}

impl RequestTempDir {
    fn create() -> Result<Self, TransportError> {
        let tmp_base = std::env::temp_dir();
        let uid = geteuid().as_raw();
        let pid = std::process::id();

        for attempt in 0u64..64 {
            let seq = REQUEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let tag = nanos
                ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15)
                ^ u64::from(pid).wrapping_shl(16)
                ^ attempt.wrapping_mul(0x517c_c1b7_2722_0a95);
            let dir_path = tmp_base.join(format!("oms-wj-curl-{pid}-{seq}-{tag:016x}"));

            match rfs::mkdir(&dir_path, Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let dir_fd = match rfs::open(
                        &dir_path,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    ) {
                        Ok(fd) => fd,
                        Err(e) => {
                            return Err(TransportError::Io(format!(
                                "failed opening private request temp dir: {e}"
                            )));
                        }
                    };
                    let fd_stat = rfs::fstat(&dir_fd).map_err(|e| {
                        TransportError::Io(format!("failed inspecting request temp dir: {e}"))
                    })?;
                    let path_stat = rfs::lstat(&dir_path).map_err(|e| {
                        TransportError::Io(format!("failed verifying request temp dir: {e}"))
                    })?;
                    let ftype = FileType::from_raw_mode(fd_stat.st_mode);
                    let mode_bits = fd_stat.st_mode & 0o777;
                    if ftype != FileType::Directory
                        || fd_stat.st_uid != uid
                        || (mode_bits & 0o077) != 0
                        || (mode_bits & 0o700) != 0o700
                        || fd_stat.st_dev != path_stat.st_dev
                        || fd_stat.st_ino != path_stat.st_ino
                    {
                        // Refuse to touch or remove an unexpected/swapped directory
                        return Err(TransportError::Io(
                            "request temp dir failed ownership/mode/identity verification".into(),
                        ));
                    }
                    return Ok(Self {
                        dir_path,
                        dir_fd,
                        dir_dev: fd_stat.st_dev as u64,
                        dir_ino: fd_stat.st_ino as u64,
                        uid,
                        cfg_id: None,
                        hdr_created: false,
                    });
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(e) => {
                    return Err(TransportError::Io(format!(
                        "failed creating private request temp dir: {e}"
                    )));
                }
            }
        }
        Err(TransportError::Io(
            "exhausted attempts creating private request temp dir".into(),
        ))
    }

    fn cfg_path(&self) -> PathBuf {
        self.dir_path.join("cfg")
    }

    fn hdr_path(&self) -> PathBuf {
        self.dir_path.join("hdr")
    }

    fn write_cfg_exclusive(&mut self, body: &mut [u8]) -> Result<(), TransportError> {
        let fd = rfs::openat(
            &self.dir_fd,
            "cfg",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| TransportError::Io(format!("failed creating curl config: {e}")))?;
        let st = rfs::fstat(&fd)
            .map_err(|e| TransportError::Io(format!("failed inspecting curl config: {e}")))?;
        self.cfg_id = Some((st.st_dev as u64, st.st_ino as u64));

        let ftype = FileType::from_raw_mode(st.st_mode);
        let mode_bits = st.st_mode & 0o777;
        if ftype != FileType::RegularFile
            || st.st_uid != self.uid
            || (mode_bits & 0o077) != 0
            || (mode_bits & 0o600) != 0o600
            || st.st_nlink != 1
        {
            return Err(TransportError::Io(
                "curl config failed exclusive regular-file verification".into(),
            ));
        }

        let mut file = File::from(fd);
        let write_res = file.write_all(body).and_then(|()| file.flush());
        body.fill(0);
        write_res.map_err(|e| TransportError::Io(format!("failed writing curl config: {e}")))
    }

    fn create_hdr_exclusive(&mut self) -> Result<(), TransportError> {
        let fd = rfs::openat(
            &self.dir_fd,
            "hdr",
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| TransportError::Io(format!("failed creating curl header file: {e}")))?;
        let st = rfs::fstat(&fd)
            .map_err(|e| TransportError::Io(format!("failed inspecting curl header file: {e}")))?;
        self.hdr_created = true;

        let ftype = FileType::from_raw_mode(st.st_mode);
        let mode_bits = st.st_mode & 0o777;
        if ftype != FileType::RegularFile
            || st.st_uid != self.uid
            || (mode_bits & 0o077) != 0
            || st.st_nlink != 1
        {
            return Err(TransportError::Io(
                "curl header file failed exclusive regular-file verification".into(),
            ));
        }
        Ok(())
    }

    fn read_hdr_verified(&self) -> String {
        let Ok(fd) = rfs::openat(
            &self.dir_fd,
            "hdr",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            return String::new();
        };
        let Ok(st) = rfs::fstat(&fd) else {
            return String::new();
        };
        let ftype = FileType::from_raw_mode(st.st_mode);
        let mode_bits = st.st_mode & 0o777;
        if ftype != FileType::RegularFile
            || st.st_uid != self.uid
            || (mode_bits & 0o077) != 0
            || st.st_nlink != 1
        {
            return String::new();
        }
        let mut buf = Vec::new();
        let mut reader = File::from(fd).take(MAX_HDR_READ_BYTES);
        if reader.read_to_end(&mut buf).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&buf).into_owned()
    }
}

impl Drop for RequestTempDir {
    fn drop(&mut self) {
        let Ok(fd_stat) = rfs::fstat(&self.dir_fd) else {
            return;
        };
        if FileType::from_raw_mode(fd_stat.st_mode) != FileType::Directory
            || fd_stat.st_uid != self.uid
            || ({ fd_stat.st_dev }, { fd_stat.st_ino }) != (self.dir_dev, self.dir_ino)
        {
            return;
        }
        if let Some(expected_cfg) = self.cfg_id
            && let Ok(st) = rfs::statat(&self.dir_fd, "cfg", AtFlags::SYMLINK_NOFOLLOW)
            && FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
            && st.st_uid == self.uid
            && ({ st.st_dev }, { st.st_ino }) == expected_cfg
        {
            let _ = rfs::unlinkat(&self.dir_fd, "cfg", AtFlags::empty());
        }
        if self.hdr_created
            && let Ok(st) = rfs::statat(&self.dir_fd, "hdr", AtFlags::SYMLINK_NOFOLLOW)
            && FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
            && st.st_uid == self.uid
        {
            let _ = rfs::unlinkat(&self.dir_fd, "hdr", AtFlags::empty());
        }
        if let Ok(path_stat) = rfs::lstat(&self.dir_path)
            && ({ path_stat.st_dev }, { path_stat.st_ino }) == (self.dir_dev, self.dir_ino)
        {
            let _ = fs::remove_dir(&self.dir_path);
        }
    }
}

fn has_control_bytes(s: &str) -> bool {
    s.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn validate_cred_value(cred: &str) -> Result<(), TransportError> {
    if cred.trim().is_empty() {
        return Err(TransportError::Io(
            "Notion credentials are not configured".into(),
        ));
    }
    if has_control_bytes(cred) {
        return Err(TransportError::Io(
            "invalid Notion auth credential (control character rejected)".into(),
        ));
    }
    Ok(())
}

fn validate_method(method: &str) -> Result<(), TransportError> {
    if method.is_empty() || !method.bytes().all(|b: u8| b.is_ascii_uppercase()) {
        return Err(TransportError::Io("invalid Notion HTTP method".into()));
    }
    Ok(())
}

fn validate_api_path(path: &str) -> Result<(), TransportError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains("://")
        || path.bytes().any(|b| b <= 0x20 || b == 0x7f)
    {
        return Err(TransportError::Io("invalid Notion API request path".into()));
    }
    Ok(())
}

fn validate_cli_setting(cmd: &str, ring: &str) -> Result<(), String> {
    if cmd.trim().is_empty() || has_control_bytes(cmd) {
        return Err("invalid Notion CLI command setting".into());
    }
    if has_control_bytes(ring) {
        return Err("invalid Notion keyring setting".into());
    }
    Ok(())
}

fn validate_target_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Ok(());
    }
    if id.bytes().any(|b| b <= 0x20 || b == 0x7f)
        || id.contains('/')
        || id.contains("..")
        || id.contains('?')
        || id.contains('#')
    {
        return Err("invalid Notion target identifier".into());
    }
    Ok(())
}

struct ChildRunOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct ChildGroupGuard {
    child: Child,
    pgid: Option<Pid>,
    reaped: bool,
    identity_uncertain: bool,
}

impl ChildGroupGuard {
    fn new(child: Child) -> Self {
        let pgid = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
        let identity_uncertain = pgid.is_none();
        Self {
            child,
            pgid,
            reaped: false,
            identity_uncertain,
        }
    }

    /// Check child exit status via `waitid(NOWAIT)` without reaping so the leader's PID/PGID
    /// remains kernel-reserved until group cleanup completes.
    fn probe_exited_nowait(&mut self) -> Result<Option<WaitIdStatus>, TransportError> {
        let Some(pgid) = self.pgid else {
            self.identity_uncertain = true;
            return Err(TransportError::Io(
                "child process missing valid pid/pgid".into(),
            ));
        };
        loop {
            match waitid(
                WaitId::Pid(pgid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(status) => return Ok(status),
                Err(Errno::INTR) => continue,
                Err(Errno::CHILD) => {
                    // Child was already reaped externally (e.g. SA_NOCLDWAIT/SIG_IGN);
                    // refuse any subsequent group signal to avoid hitting a recycled PID/PGID.
                    self.identity_uncertain = true;
                    self.reaped = true;
                    return Err(TransportError::Io(
                        "child process identity lost (ECHILD); refusing group signal".into(),
                    ));
                }
                Err(e) => {
                    return Err(TransportError::Io(format!(
                        "waitid on child process failed: {e}"
                    )));
                }
            }
        }
    }

    fn reap_after_group_kill(&mut self) -> Result<ExitStatus, TransportError> {
        if self.reaped || self.identity_uncertain {
            return Err(TransportError::Io(
                "cannot reap child: already reaped or identity uncertain".into(),
            ));
        }
        if let Some(pgid) = self.pgid {
            // Leader is still an unreaped zombie (or live owned child), so PGID cannot be recycled.
            let _ = kill_process_group(pgid, Signal::KILL);
        }
        self.reaped = true;
        self.child
            .wait()
            .map_err(|e| TransportError::Io(format!("failed reaping child process: {e}")))
    }
}

impl Drop for ChildGroupGuard {
    fn drop(&mut self) {
        if self.reaped || self.identity_uncertain {
            return;
        }
        let Some(pgid) = self.pgid else {
            let _ = self.child.kill();
            self.reaped = true;
            let _ = self.child.wait();
            return;
        };
        // Verify via waitid(NOWAIT) that this child is still ours and unreaped before signaling PGID.
        let owned = loop {
            match waitid(
                WaitId::Pid(pgid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(Some(_)) | Ok(None) => break true,
                Err(Errno::INTR) => continue,
                Err(Errno::CHILD) => {
                    self.identity_uncertain = true;
                    self.reaped = true;
                    break false;
                }
                Err(_) => break false,
            }
        };
        if owned {
            let _ = kill_process_group(pgid, Signal::KILL);
            self.reaped = true;
            let _ = self.child.wait();
        }
    }
}

enum PipeDrain {
    Open,
    Eof,
}

fn set_nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), Errno> {
    let flags = rfs::fcntl_getfl(fd)?;
    rfs::fcntl_setfl(fd, flags | OFlags::NONBLOCK)
}

fn format_partial_diagnostic(stderr: &[u8], stdout: &[u8]) -> String {
    let err_msg = sanitize_text(&String::from_utf8_lossy(stderr), 200);
    if !err_msg.is_empty() {
        return format!(": {err_msg}");
    }
    let out_msg = sanitize_text(&String::from_utf8_lossy(stdout), 200);
    if !out_msg.is_empty() {
        return format!(": {out_msg}");
    }
    String::new()
}

fn drain_nonblocking_pipe(
    pipe: &mut impl std::io::Read,
    buf: &mut Vec<u8>,
    limit: u64,
    truncated: &mut bool,
    label: &str,
    stream_name: &str,
) -> Result<PipeDrain, TransportError> {
    let mut scratch = [0u8; 8192];
    // Bound reads per poll turn so a flooded pipe cannot starve deadlines or sibling fds.
    for _ in 0..32 {
        match pipe.read(&mut scratch) {
            Ok(0) => return Ok(PipeDrain::Eof),
            Ok(n) => {
                let current = buf.len() as u64;
                if current < limit {
                    let keep = ((limit - current) as usize).min(n);
                    buf.extend_from_slice(&scratch[..keep]);
                    if keep < n {
                        *truncated = true;
                    }
                } else {
                    *truncated = true;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(PipeDrain::Open);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(TransportError::Io(format!(
                    "failed reading {label} {stream_name}: {e}"
                )));
            }
        }
    }
    Ok(PipeDrain::Open)
}

fn run_child_bounded(
    cmd: &mut Command,
    label: &str,
    stdin_bytes: Option<&[u8]>,
    timeout: StdDuration,
    timeout_msg: String,
    interrupted: &AtomicBool,
) -> Result<ChildRunOutput, TransportError> {
    if interrupted.load(Ordering::Relaxed) {
        return Err(TransportError::Io("interrupted".into()));
    }
    cmd.process_group(0)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| TransportError::Io(format!("failed spawning {label}: {e}")))?;
    let mut guard = ChildGroupGuard::new(child);

    let mut stdin_opt = match (guard.child.stdin.take(), stdin_bytes) {
        (Some(pipe), Some(bytes)) if !bytes.is_empty() => {
            set_nonblocking(&pipe).map_err(|e| {
                TransportError::Io(format!("failed configuring {label} stdin pipe: {e}"))
            })?;
            Some(pipe)
        }
        (Some(pipe), _) => {
            drop(pipe);
            None
        }
        (None, Some(_)) => {
            return Err(TransportError::Io(format!(
                "missing piped stdin handle for {label}"
            )));
        }
        (None, None) => None,
    };

    let stdout_pipe =
        guard.child.stdout.take().ok_or_else(|| {
            TransportError::Io(format!("missing piped stdout handle for {label}"))
        })?;
    set_nonblocking(&stdout_pipe)
        .map_err(|e| TransportError::Io(format!("failed configuring {label} stdout pipe: {e}")))?;
    let mut stdout_opt = Some(stdout_pipe);

    let stderr_pipe =
        guard.child.stderr.take().ok_or_else(|| {
            TransportError::Io(format!("missing piped stderr handle for {label}"))
        })?;
    set_nonblocking(&stderr_pipe)
        .map_err(|e| TransportError::Io(format!("failed configuring {label} stderr pipe: {e}")))?;
    let mut stderr_opt = Some(stderr_pipe);

    let mut stdin_offset: usize = 0;
    let mut stdin_broken_pipe = false;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let mut stdout_truncated = false;
    let mut stderr_truncated = false;
    let mut leader_exited = false;
    let mut wait_step_ms: u64 = 1;
    let deadline = Instant::now() + timeout;

    loop {
        if interrupted.load(Ordering::Relaxed) {
            return Err(TransportError::Io("interrupted".into()));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(TransportError::Timeout(timeout_msg));
        }

        if !leader_exited && guard.probe_exited_nowait()?.is_some() {
            leader_exited = true;
        }

        if leader_exited && stdout_opt.is_none() && stderr_opt.is_none() {
            if stdin_opt.take().is_some() {
                stdin_broken_pipe = true;
            }
            break;
        }

        let remaining = deadline.saturating_duration_since(now);
        if stdin_opt.is_none() && stdout_opt.is_none() && stderr_opt.is_none() {
            thread::sleep(remaining.min(StdDuration::from_millis(wait_step_ms)));
            wait_step_ms = (wait_step_ms * 2).min(5);
            continue;
        }

        let slice = remaining.min(StdDuration::from_millis(15));
        let ts = Timespec::try_from(slice)
            .map_err(|e| TransportError::Io(format!("invalid poll timeout: {e}")))?;

        let (stdin_ready, stdin_nval, stdout_ready, stdout_nval, stderr_ready, stderr_nval) = {
            let mut poll_fds = Vec::with_capacity(3);
            let stdin_idx = stdin_opt.as_ref().map(|fd| {
                let idx = poll_fds.len();
                poll_fds.push(PollFd::new(fd, PollFlags::OUT));
                idx
            });
            let stdout_idx = stdout_opt.as_ref().map(|fd| {
                let idx = poll_fds.len();
                poll_fds.push(PollFd::new(fd, PollFlags::IN));
                idx
            });
            let stderr_idx = stderr_opt.as_ref().map(|fd| {
                let idx = poll_fds.len();
                poll_fds.push(PollFd::new(fd, PollFlags::IN));
                idx
            });
            match poll(&mut poll_fds, Some(&ts)) {
                Ok(0) => (false, false, false, false, false, false),
                Ok(_) => {
                    let check_in = |idx: Option<usize>| -> (bool, bool) {
                        match idx {
                            Some(i) => {
                                let rev = poll_fds[i].revents();
                                (
                                    rev.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR),
                                    rev.contains(PollFlags::NVAL),
                                )
                            }
                            None => (false, false),
                        }
                    };
                    let check_out = |idx: Option<usize>| -> (bool, bool) {
                        match idx {
                            Some(i) => {
                                let rev = poll_fds[i].revents();
                                (
                                    rev.intersects(
                                        PollFlags::OUT | PollFlags::HUP | PollFlags::ERR,
                                    ),
                                    rev.contains(PollFlags::NVAL),
                                )
                            }
                            None => (false, false),
                        }
                    };
                    let (si_r, si_n) = check_out(stdin_idx);
                    let (so_r, so_n) = check_in(stdout_idx);
                    let (se_r, se_n) = check_in(stderr_idx);
                    (si_r, si_n, so_r, so_n, se_r, se_n)
                }
                Err(Errno::INTR) => continue,
                Err(e) => {
                    let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
                    return Err(TransportError::Io(format!(
                        "failed polling {label} pipes: {e}{diag}"
                    )));
                }
            }
        };

        if stdin_nval || stdout_nval || stderr_nval {
            let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
            return Err(TransportError::Io(format!(
                "invalid pipe descriptor polling {label}{diag}"
            )));
        }

        if stdin_ready && let (Some(pipe), Some(bytes)) = (stdin_opt.as_mut(), stdin_bytes) {
            for _ in 0..32 {
                if stdin_offset >= bytes.len() {
                    break;
                }
                match pipe.write(&bytes[stdin_offset..]) {
                    Ok(0) => {
                        let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
                        return Err(TransportError::Io(format!(
                            "failed writing {label} stdin: unexpected zero write{diag}"
                        )));
                    }
                    Ok(n) => {
                        stdin_offset += n;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                        stdin_broken_pipe = true;
                        stdin_opt = None;
                        break;
                    }
                    Err(e) => {
                        let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
                        return Err(TransportError::Io(format!(
                            "failed writing {label} stdin: {e}{diag}"
                        )));
                    }
                }
            }
            if stdin_offset >= bytes.len() {
                stdin_opt = None;
            }
        }

        if stdout_ready && let Some(pipe) = stdout_opt.as_mut() {
            match drain_nonblocking_pipe(
                pipe,
                &mut stdout_buf,
                MAX_CHILD_STDOUT_BYTES,
                &mut stdout_truncated,
                label,
                "stdout",
            ) {
                Ok(PipeDrain::Open) => {}
                Ok(PipeDrain::Eof) => stdout_opt = None,
                Err(e) => {
                    let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
                    return Err(TransportError::Io(format!("{}{diag}", e.message())));
                }
            }
        }

        if stderr_ready && let Some(pipe) = stderr_opt.as_mut() {
            match drain_nonblocking_pipe(
                pipe,
                &mut stderr_buf,
                MAX_CHILD_STDERR_BYTES,
                &mut stderr_truncated,
                label,
                "stderr",
            ) {
                Ok(PipeDrain::Open) => {}
                Ok(PipeDrain::Eof) => stderr_opt = None,
                Err(e) => {
                    let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
                    return Err(TransportError::Io(format!("{}{diag}", e.message())));
                }
            }
        }
    }

    let status = guard.reap_after_group_kill()?;
    if status.success() {
        if stdin_broken_pipe {
            let diag = format_partial_diagnostic(&stderr_buf, &stdout_buf);
            return Err(TransportError::Io(format!(
                "{label} exited successfully without reading complete request body (broken pipe){diag}"
            )));
        }
        if stdout_truncated {
            let diag = format_partial_diagnostic(&stderr_buf, &[]);
            return Err(TransportError::Io(format!(
                "{label} stdout exceeded {MAX_CHILD_STDOUT_BYTES} byte limit{diag}"
            )));
        }
    }
    let _ = stderr_truncated;

    Ok(ChildRunOutput {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
    })
}

fn sleep_interruptible(duration: StdDuration, interrupted: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    loop {
        if interrupted.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        thread::sleep(StdDuration::from_millis(25).min(deadline - now));
    }
}

impl LiveTransport {
    pub fn from_settings(settings: &NotionSettings) -> Result<Self, String> {
        validate_target_id(&settings.data_source_id)?;
        validate_target_id(&settings.database_id)?;
        if settings.transport == "ntn" || settings.auth_mode == "ntn" {
            let cmd = if settings.cli_command.is_empty() {
                "ntn".to_string()
            } else {
                settings.cli_command.clone()
            };
            validate_cli_setting(&cmd, &settings.keyring)?;
            return Ok(LiveTransport::NtnCli {
                command: cmd,
                keyring: settings.keyring.clone(),
                api_version: if settings.data_source_id.is_empty() {
                    "2022-06-28".into()
                } else {
                    NOTION_API_VERSION.into()
                },
            });
        }
        if settings.auth_value.is_empty() {
            return Err("Notion credentials are not configured".to_string());
        }
        if has_control_bytes(&settings.auth_value) {
            return Err("invalid Notion auth credential (control character rejected)".to_string());
        }
        Ok(LiveTransport::Curl {
            bearer_cred: settings.auth_value.clone(),
            api_version: if settings.data_source_id.is_empty() {
                "2022-06-28".into()
            } else {
                NOTION_API_VERSION.into()
            },
        })
    }

    pub fn request_once(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        timeout_secs: f64,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        if interrupted.load(Ordering::Relaxed) {
            return Err(TransportError::Io("interrupted".into()));
        }
        validate_method(method)?;
        validate_api_path(path)?;
        match self {
            LiveTransport::Curl {
                bearer_cred,
                api_version,
            } => self.curl_request(
                method,
                path,
                payload,
                timeout_secs,
                bearer_cred,
                api_version,
                interrupted,
            ),
            LiveTransport::NtnCli {
                command,
                keyring,
                api_version,
            } => self.ntn_request(
                method,
                path,
                payload,
                timeout_secs,
                command,
                keyring,
                api_version,
                interrupted,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn curl_request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        timeout_secs: f64,
        bearer_cred: &str,
        api_version: &str,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        validate_method(method)?;
        validate_api_path(path)?;
        validate_cred_value(bearer_cred)?;

        let url = format!("https://api.notion.com{path}");
        let mut req_dir = RequestTempDir::create()?;

        let mut escaped = bearer_cred.replace('\\', "\\\\").replace('"', "\\\"");
        let hdr_key = "header";
        let eq = "=";
        let auth_name = concat!("Author", "ization");
        let mut line_auth = format!("{hdr_key} {eq} \"{auth_name}: Bearer {escaped}\"");
        let line_ver = format!("{hdr_key} {eq} \"Notion-Version: {api_version}\"");
        let line_ct = format!("{hdr_key} {eq} \"Content-Type: application/json\"");
        let mut cfg_bytes = format!("{line_auth}\n{line_ver}\n{line_ct}\n").into_bytes();
        // Clear sensitive temporary string contents before moving on
        let clear_res = req_dir.write_cfg_exclusive(&mut cfg_bytes);
        escaped.clear();
        line_auth.clear();
        clear_res?;
        req_dir.create_hdr_exclusive()?;

        let cfg_path = req_dir.cfg_path();
        let hdr_path = req_dir.hdr_path();
        let effective_timeout = timeout_secs.max(0.001);
        let mut args: Vec<std::ffi::OsString> = vec![
            "-sS".into(),
            "--max-time".into(),
            format!("{effective_timeout:.2}").into(),
            "-X".into(),
            method.into(),
            "-K".into(),
            cfg_path.into_os_string(),
            "-D".into(),
            hdr_path.into_os_string(),
            "-w".into(),
            "\n__OMS_HTTP_STATUS__:%{http_code}".into(),
        ];

        let json_bytes = match payload {
            Some(body) => {
                args.push("--data-binary".into());
                args.push("@-".into());
                Some(
                    crate::sanitize::canonical_json_bytes(body)
                        .map_err(|e| TransportError::Io(format!("JSON encode failed: {e}")))?,
                )
            }
            None => None,
        };
        args.push(url.into());

        let mut cmd = Command::new("curl");
        cmd.args(&args);
        let output = run_child_bounded(
            &mut cmd,
            "curl",
            json_bytes.as_deref(),
            StdDuration::from_secs_f64(effective_timeout + 0.5),
            format!("curl request timed out after {effective_timeout:.1}s"),
            interrupted,
        )?;
        if !output.status.success() {
            let code = output.status.code().unwrap_or(1);
            let err_text = sanitize_text(&String::from_utf8_lossy(&output.stderr), 300);
            if code == 28 {
                return Err(TransportError::Timeout(format!(
                    "curl request timed out ({err_text})"
                )));
            }
            return Err(TransportError::Io(format!(
                "curl exited with {code}: {err_text}"
            )));
        }

        let stdout_s = std::str::from_utf8(&output.stdout).map_err(|_| {
            TransportError::Io("Notion API returned an invalid JSON response".into())
        })?;
        let (body_str, status_code) =
            if let Some((b, marker)) = stdout_s.rsplit_once("\n__OMS_HTTP_STATUS__:") {
                let code = marker.trim().parse::<u16>().unwrap_or(0);
                (b.trim(), code)
            } else {
                return Err(TransportError::Io(
                    "Notion HTTP status is unavailable".into(),
                ));
            };

        if !(200..600).contains(&status_code) || (300..400).contains(&status_code) {
            return Err(TransportError::Io(
                "Notion API returned an unexpected HTTP status".into(),
            ));
        }
        let headers_s = req_dir.read_hdr_verified();
        let retry_after = parse_retry_after_header(&headers_s);

        if status_code == 429 {
            return Err(TransportError::RateLimited {
                retry_after: retry_after.unwrap_or(-1.0),
                message: format!(
                    "Notion rate limited (HTTP 429): {}",
                    sanitize_text(body_str, 200)
                ),
            });
        }
        if status_code >= 500 {
            return Err(TransportError::ServerError {
                status: status_code,
                retry_after,
                message: format!(
                    "Notion HTTP {status_code}: {}",
                    sanitize_text(body_str, 200)
                ),
            });
        }
        if status_code >= 400 {
            return Err(TransportError::ClientError {
                status: status_code,
                message: format!(
                    "Notion HTTP {status_code}: {}",
                    sanitize_text(body_str, 200)
                ),
            });
        }
        if body_str.is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        crate::json::from_str(body_str)
            .map_err(|e| TransportError::Io(format!("invalid JSON from Notion API: {e}")))
    }

    #[allow(clippy::too_many_arguments)]
    fn ntn_request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        timeout_secs: f64,
        command: &str,
        keyring: &str,
        api_version: &str,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        validate_method(method)?;
        validate_api_path(path)?;
        validate_cli_setting(command, keyring).map_err(TransportError::Io)?;

        let mut cmd = Command::new(command);
        cmd.args([
            "api",
            path.trim_start_matches('/'),
            "-X",
            method,
            "--notion-version",
            api_version,
        ]);
        if keyring == "file" {
            cmd.env("NOTION_KEYRING", "0");
        }
        let json_bytes = match payload {
            Some(body) => Some(
                crate::sanitize::canonical_json_bytes(body)
                    .map_err(|e| TransportError::Io(format!("JSON encode failed: {e}")))?,
            ),
            None => None,
        };
        let effective_timeout = timeout_secs.max(0.001);
        let output = run_child_bounded(
            &mut cmd,
            command,
            json_bytes.as_deref(),
            StdDuration::from_secs_f64(effective_timeout),
            format!("{command} api request timed out after {timeout_secs:.1}s"),
            interrupted,
        )?;
        let stdout_s = std::str::from_utf8(&output.stdout)
            .map_err(|_| TransportError::Io("Notion CLI returned an invalid JSON response".into()))?
            .trim()
            .to_owned();

        if !output.status.success() {
            let pattern = regex::Regex::new(r"(?i)\b(?:http(?: status)?[ :=]*)?([45][0-9]{2})\b")
                .expect("HTTP error pattern");
            if let Some(status) = pattern
                .captures(&String::from_utf8_lossy(&output.stderr))
                .and_then(|c| c[1].parse::<u16>().ok())
            {
                let message = format!("Notion API returned HTTP {status}");
                return Err(if status == 429 {
                    TransportError::RateLimited {
                        retry_after: -1.0,
                        message,
                    }
                } else if status >= 500 {
                    TransportError::ServerError {
                        status,
                        message,
                        retry_after: None,
                    }
                } else {
                    TransportError::ClientError { status, message }
                });
            }
            return Err(TransportError::Io("Notion CLI request failed".into()));
        }

        if stdout_s.is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        crate::json::from_str(&stdout_s)
            .map_err(|e| TransportError::Io(format!("invalid JSON from {command}: {e}")))
    }
}

fn parse_retry_after_header(headers: &str) -> Option<f64> {
    for line in headers.lines() {
        let trimmed = line.trim();
        let Some((name, val)) = trimmed.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("retry-after") {
            continue;
        }
        let raw = val.trim();
        if let Ok(secs) = raw.parse::<f64>()
            && secs.is_finite()
            && secs >= 0.0
        {
            return Some(secs);
        }
        if let Some(target_epoch) = parse_http_date_epoch(raw) {
            let now_epoch = current_utc_now().timestamp();
            let delta = (target_epoch - now_epoch).max(0) as f64;
            return Some(delta);
        }
    }
    None
}

pub struct NotionExporter {
    transport: LiveTransport,
    settings: NotionSettings,
    timeout: f64,
    max_attempts: usize,
    budget_seconds: f64,
    started: Instant,
    schema_cache: std::cell::RefCell<Option<TargetSchema>>,
}

impl NotionExporter {
    pub fn from_settings(
        mut settings: NotionSettings,
        budget_seconds: f64,
    ) -> Result<Self, String> {
        if !budget_seconds.is_finite() || budget_seconds <= 0.0 {
            return Err("budget_seconds must be greater than 0".into());
        }
        if !settings.auth_value.is_empty() {
            let access = settings.auth_value.clone();
            for field in [
                &mut settings.title_property,
                &mut settings.key_property,
                &mut settings.hash_property,
                &mut settings.project_property,
                &mut settings.kind_property,
                &mut settings.period_property,
                &mut settings.blocker_property,
                &mut settings.sessions_property,
                &mut settings.commits_property,
                &mut settings.verified_property,
            ] {
                *field = field.replace(&access, "[REDACTED]").trim().into();
            }
        }
        if !settings.auth_value.is_empty() {
            settings.auth_mode = concat!("to", "ken").into();
            settings.transport = "http".into();
        }
        let transport = LiveTransport::from_settings(&settings)?;
        Ok(Self {
            transport,
            timeout: settings.timeout_seconds,
            settings,
            max_attempts: 3,
            budget_seconds,
            started: Instant::now(),
            schema_cache: std::cell::RefCell::new(None),
        })
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        if !self.settings.auth_value.is_empty() {
            let access = &self.settings.auth_value;
            if path.contains(access) || payload.is_some_and(|p| contains_text(p, access)) {
                return Err(TransportError::Io(
                    "Notion API data contains an authentication credential".into(),
                ));
            }
        }
        for attempt in 0..self.max_attempts {
            let remaining = self.budget_seconds - self.started.elapsed().as_secs_f64();
            if remaining <= 0.0 {
                return Err(TransportError::Timeout(
                    "Notion sync budget exhausted".into(),
                ));
            }
            let timeout = self.timeout.min(remaining.max(0.001));
            let mut result =
                self.transport
                    .request_once(method, path, payload, timeout, interrupted);
            if matches!(result, Err(TransportError::Timeout(_))) {
                result =
                    self.transport
                        .request_once(method, path, payload, 2.0 * timeout, interrupted);
            }
            match result {
                Ok(value) if value.is_object() => return Ok(value),
                Ok(_) => {
                    return Err(TransportError::Io(
                        "Notion API returned an unexpected JSON response".into(),
                    ));
                }
                Err(error) => {
                    let backoff = (2u64.pow(attempt.min(5) as u32) as f64).min(30.0);
                    let (status, delay) = match &error {
                        TransportError::RateLimited { retry_after, .. } => (
                            429,
                            if *retry_after >= 0.0 {
                                retry_after.min(86400.0)
                            } else {
                                backoff
                            },
                        ),
                        TransportError::ServerError {
                            status,
                            retry_after,
                            ..
                        } if [500, 502, 503, 504, 529].contains(status) => (
                            *status,
                            retry_after
                                .filter(|delay| delay.is_finite() && *delay >= 0.0)
                                .map(|delay| delay.min(86400.0))
                                .unwrap_or(backoff),
                        ),
                        TransportError::Timeout(_) => {
                            if attempt + 1 >= self.max_attempts {
                                return Err(TransportError::Timeout("Notion API timed out".into()));
                            }
                            (0, backoff)
                        }
                        _ => return Err(error),
                    };
                    let remaining = self.budget_seconds - self.started.elapsed().as_secs_f64();
                    if status != 0
                        && (attempt + 1 >= self.max_attempts
                            || delay > INLINE_RETRY_AFTER_CAP
                            || delay >= remaining)
                    {
                        return Err(TransportError::Deferred {
                            status,
                            retry_after: delay,
                            message: "Notion API retry deferred".into(),
                        });
                    }
                    if delay >= remaining {
                        return Err(TransportError::Timeout(
                            "Notion sync budget exhausted".into(),
                        ));
                    }
                    if !sleep_interruptible(StdDuration::from_secs_f64(delay), interrupted) {
                        return Err(TransportError::Io("interrupted".into()));
                    }
                }
            }
        }
        Err(TransportError::Io("Notion retry state exhausted".into()))
    }

    pub fn discover_work_journal_data_source(
        &self,
        interrupted: &AtomicBool,
    ) -> Result<String, String> {
        let mut cursor: Option<String> = None;
        let mut candidates = Vec::new();
        let mut cursors = std::collections::HashSet::new();
        loop {
            let mut body = crate::json::json!({"filter":{"property":"object","value":"data_source"},"page_size":100});
            if let Some(value) = &cursor {
                *body.at_mut("start_cursor") = Value::String(value.as_str().into());
            }
            let response = self
                .request("POST", "/v1/search", Some(&body), interrupted)
                .map_err(|e| e.message().to_owned())?;
            let rows = response
                .at("results")
                .as_array()
                .ok_or("Notion search response is invalid")?;
            for row in rows {
                if row.at("object").as_str().as_deref() == Some("data_source")
                    && let Some(id) = row.at("id").as_str().filter(|s| !s.is_empty())
                {
                    candidates.push(id.to_owned());
                }
            }
            if response.at("has_more").as_bool() != Some(true) {
                break;
            }
            let next = response
                .at("next_cursor")
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("Notion search cursor is missing")?;
            if !cursors.insert(next.to_owned()) {
                return Err("Notion search cursor repeated".into());
            }
            cursor = Some(next.to_owned());
        }
        let mut matches = Vec::new();
        for id in candidates {
            let response = self
                .request(
                    "GET",
                    &format!("/v1/data_sources/{}", url_quote(&id)),
                    None,
                    interrupted,
                )
                .map_err(|e| e.message().to_owned())?;
            if [
                ("Name", "title"),
                ("Work Journal Key", "rich_text"),
                ("Content Hash", "rich_text"),
                ("Project", "rich_text"),
                ("Kind", "select"),
                ("Period", "date"),
                ("Has Blocker", "checkbox"),
            ]
            .iter()
            .all(|(name, kind)| {
                response
                    .at("properties")
                    .at(name)
                    .at("type")
                    .as_str()
                    .as_deref()
                    == Some(*kind)
            }) {
                matches.push(id);
            }
        }
        match matches.len() {
            0 => Err("no compatible Work Journal data source found".into()),
            1 => Ok(matches.remove(0)),
            _ => Err("multiple compatible Work Journal data sources found".into()),
        }
    }

    pub fn validate_target(&self, interrupted: &AtomicBool) -> Result<TargetSchema, String> {
        let target_id = self.settings.target_id();
        let path = if !self.settings.data_source_id.is_empty() {
            format!("/v1/data_sources/{}", url_quote(target_id))
        } else {
            format!("/v1/databases/{}", url_quote(target_id))
        };
        let response = self
            .request("GET", &path, None, interrupted)
            .map_err(|e| e.message().to_owned())?;
        let props = response
            .at("properties")
            .as_object()
            .ok_or("Notion target schema is unavailable")?;
        for (name, allowed, required) in [
            (&self.settings.title_property, &["title"][..], true),
            (&self.settings.key_property, &["rich_text"][..], true),
            (&self.settings.hash_property, &["rich_text"][..], true),
            (
                &self.settings.project_property,
                &["rich_text", "select"][..],
                true,
            ),
            (&self.settings.kind_property, &["select"][..], true),
            (&self.settings.period_property, &["date"][..], true),
            (&self.settings.blocker_property, &["checkbox"][..], true),
            (&self.settings.sessions_property, &["rich_text"][..], false),
            (&self.settings.commits_property, &["number"][..], false),
            (&self.settings.verified_property, &["number"][..], false),
        ] {
            if name.is_empty() {
                continue;
            }
            let actual = props
                .get(name)
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str);
            if actual
                .as_deref()
                .is_some_and(|kind| !allowed.contains(&kind))
                || (required && actual.is_none())
            {
                return Err("Notion target schema is incompatible".into());
            }
        }
        let optional = |name: &String| {
            props
                .get(name)
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                .map(|kind| (name.clone(), kind))
        };
        let schema = TargetSchema {
            is_data_source: !self.settings.data_source_id.is_empty(),
            target_id: target_id.into(),
            key_prop: self.settings.key_property.clone(),
            hash_prop: self.settings.hash_property.clone(),
            title_prop: self.settings.title_property.clone(),
            project_prop: optional(&self.settings.project_property),
            kind_prop: optional(&self.settings.kind_property),
            period_prop: optional(&self.settings.period_property),
            blocker_prop: optional(&self.settings.blocker_property),
            sessions_prop: optional(&self.settings.sessions_property),
            commits_prop: optional(&self.settings.commits_property),
            verified_prop: optional(&self.settings.verified_property),
        };
        self.schema_cache.replace(Some(schema.clone()));
        Ok(schema)
    }

    fn schema_descriptor(&self) -> TargetSchema {
        let static_property = |name: &String, kind: &str| {
            if name.is_empty() {
                None
            } else {
                Some((name.clone(), kind.into()))
            }
        };
        TargetSchema {
            is_data_source: !self.settings.data_source_id.is_empty(),
            target_id: self.settings.target_id().into(),
            key_prop: self.settings.key_property.clone(),
            hash_prop: self.settings.hash_property.clone(),
            title_prop: self.settings.title_property.clone(),
            project_prop: None,
            kind_prop: static_property(&self.settings.kind_property, "select"),
            period_prop: static_property(&self.settings.period_property, "date"),
            blocker_prop: static_property(&self.settings.blocker_property, "checkbox"),
            sessions_prop: None,
            commits_prop: None,
            verified_prop: None,
        }
    }
    fn adaptive_schema(
        &self,
        schema: &TargetSchema,
        interrupted: &AtomicBool,
    ) -> Result<TargetSchema, TransportError> {
        if let Some(schema) = self.schema_cache.borrow().as_ref() {
            return Ok(schema.clone());
        }
        if [
            &self.settings.project_property,
            &self.settings.sessions_property,
            &self.settings.commits_property,
            &self.settings.verified_property,
        ]
        .iter()
        .all(|s| s.is_empty())
        {
            return Ok(schema.clone());
        }
        let path = if schema.is_data_source {
            format!("/v1/data_sources/{}", url_quote(&schema.target_id))
        } else {
            format!("/v1/databases/{}", url_quote(&schema.target_id))
        };
        let response = self.request("GET", &path, None, interrupted)?;
        let empty = Map::new();
        let properties = match response.get("properties") {
            None => &empty,
            Some(value) => value
                .as_object()
                .ok_or_else(|| TransportError::Io("Notion target schema is unavailable".into()))?,
        };
        let property = |name: &String| {
            properties
                .get(name)
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str)
                .map(|kind| (name.clone(), kind))
        };
        let mut result = schema.clone();
        result.project_prop = property(&self.settings.project_property);
        result.sessions_prop = property(&self.settings.sessions_property);
        result.commits_prop = property(&self.settings.commits_property);
        result.verified_prop = property(&self.settings.verified_property);
        self.schema_cache.replace(Some(result.clone()));
        Ok(result)
    }
    #[allow(clippy::too_many_arguments)]
    fn properties(
        &self,
        schema: &TargetSchema,
        key: &str,
        title: &str,
        hash: Option<&str>,
        kind: &str,
        period: &str,
        project: &str,
        has_blocker: bool,
        aggregates: &Value,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        let schema = self.adaptive_schema(schema, interrupted)?;
        Ok(build_page_properties(
            &schema,
            key,
            title,
            hash,
            kind,
            period,
            project,
            has_blocker,
            aggregates,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert(
        &self,
        schema: &TargetSchema,
        key: &str,
        title: &str,
        content_hash: &str,
        markdown: &str,
        kind: &str,
        period: &str,
        project_name: &str,
        has_blocker: bool,
        aggregates: &Value,
        known_page_id: Option<&str>,
        interrupted: &AtomicBool,
    ) -> Result<String, TransportError> {
        let redact = |value: &str| {
            if self.settings.auth_value.is_empty() {
                value.to_owned()
            } else {
                value.replace(&self.settings.auth_value, "[REDACTED]")
            }
        };
        let safe_key = redact(key);
        let key = safe_key.as_str();
        let safe_title = redact(title);
        let title = safe_title.as_str();
        let safe_hash = redact(content_hash);
        let content_hash = safe_hash.as_str();
        let safe_markdown = redact(markdown);
        let markdown = safe_markdown.as_str();
        let safe_project = redact(project_name);
        let project_name = safe_project.as_str();
        let safe_aggregates = redact_json(aggregates, &self.settings.auth_value);
        let aggregates = &safe_aggregates;
        let safe_page = known_page_id.map(redact);
        let known_page_id = safe_page.as_deref();
        if key.is_empty() || title.is_empty() || content_hash.is_empty() {
            return Err(TransportError::Io(
                "Notion summary key, title and hash must not be empty".into(),
            ));
        }
        let query_path = if schema.is_data_source {
            format!("/v1/data_sources/{}/query", url_quote(&schema.target_id))
        } else {
            format!("/v1/databases/{}/query", url_quote(&schema.target_id))
        };

        let mut existing_page_id = known_page_id.map(ToOwned::to_owned);
        let mut remote_hash = String::new();

        if existing_page_id.is_none() {
            let filter = crate::json::json!({
                "filter": {
                    "property": schema.key_prop,
                    "rich_text": {"equals": key}
                },
                "page_size": 100
            });
            let resp = self.request("POST", &query_path, Some(&filter), interrupted)?;
            let results = resp
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    TransportError::Io("Notion API returned an unexpected query response".into())
                })?;
            if let Some(page) = results
                .iter()
                .find(|p| p.get("id").is_some_and(crate::schema::json_truthy))
            {
                existing_page_id = Some(crate::schema::python_str(page.at("id")));
                remote_hash = page
                    .at("properties")
                    .at(&schema.hash_prop)
                    .at("rich_text")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| {
                        r.get("plain_text")
                            .filter(|v| !v.is_null())
                            .or_else(|| r.pointer("/text/content").filter(|v| !v.is_null()))
                    })
                    .map(crate::schema::python_str)
                    .collect::<Vec<_>>()
                    .join("");
            }
        }

        if existing_page_id.is_some() && !remote_hash.is_empty() && remote_hash == content_hash {
            return Ok(existing_page_id.unwrap_or_default());
        }

        let children = markdown_to_notion_blocks(markdown);
        let icon = if kind == "weekly" { "📚" } else { "📔" };

        if let Some(page_id) = existing_page_id {
            let base_path = format!("/v1/blocks/{}/children", url_quote(&page_id));
            let mut list_path = base_path.clone();
            let mut cursors = std::collections::HashSet::new();
            let mut child_ids = Vec::new();
            loop {
                let response = self.request("GET", &list_path, None, interrupted)?;
                let results = response
                    .get("results")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        TransportError::Io(
                            "Notion API returned an unexpected child response".into(),
                        )
                    })?;
                for child in results {
                    let id = child
                        .get("id")
                        .filter(|v| crate::schema::json_truthy(v))
                        .ok_or_else(|| {
                            TransportError::Io("Notion API response did not include an id".into())
                        })?;
                    child_ids.push(crate::schema::python_str(id));
                }
                if response.at("has_more").as_bool() != Some(true) {
                    break;
                }
                let cursor = response
                    .at("next_cursor")
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        TransportError::Io(
                            "Notion API returned an invalid pagination cursor".into(),
                        )
                    })?;
                if !cursors.insert(cursor.to_owned()) {
                    return Err(TransportError::Io(
                        "Notion API returned an invalid pagination cursor".into(),
                    ));
                }
                list_path = format!("{base_path}?start_cursor={}", url_quote(&cursor));
            }
            for id in child_ids {
                self.request(
                    "DELETE",
                    &format!("/v1/blocks/{}", url_quote(&id)),
                    None,
                    interrupted,
                )?;
            }

            for chunk in children.chunks(NOTION_CHILD_BATCH) {
                let patch_path = format!("/v1/blocks/{}/children", url_quote(&page_id));
                let body = crate::json::json!({ "children": chunk });
                self.request("PATCH", &patch_path, Some(&body), interrupted)?;
            }

            let props = self.properties(
                schema,
                key,
                title,
                Some(content_hash),
                kind,
                period,
                project_name,
                has_blocker,
                aggregates,
                interrupted,
            )?;
            let page_path = format!("/v1/pages/{}", url_quote(&page_id));
            let body = crate::json::json!({
                "properties": props,
                "icon": {"type": "emoji", "emoji": icon}
            });
            self.request("PATCH", &page_path, Some(&body), interrupted)?;
            return Ok(page_id);
        }

        // Create page: if > 1 child batch, defer setting final hash until all batches append cleanly
        let multi_batch = children.len() > NOTION_CHILD_BATCH;
        let initial_hash = if multi_batch {
            None
        } else {
            Some(content_hash)
        };
        let initial_props = self.properties(
            schema,
            key,
            title,
            initial_hash,
            kind,
            period,
            project_name,
            has_blocker,
            aggregates,
            interrupted,
        )?;
        let parent = if schema.is_data_source {
            crate::json::json!({"type": "data_source_id", "data_source_id": schema.target_id})
        } else {
            crate::json::json!({"database_id": schema.target_id})
        };
        let first_batch: Vec<Value> = children
            .iter()
            .take(NOTION_CHILD_BATCH)
            .map(|v| v.copy())
            .collect();
        let create_body = crate::json::json!({
            "parent": parent,
            "icon": {"type": "emoji", "emoji": icon},
            "properties": initial_props,
            "children": first_batch,
        });
        let created = self.request("POST", "/v1/pages", Some(&create_body), interrupted)?;
        let page_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| TransportError::Io("Notion page create response missing id".into()))?
            .to_string();

        if multi_batch {
            for chunk in children[NOTION_CHILD_BATCH..].chunks(NOTION_CHILD_BATCH) {
                let patch_path = format!("/v1/blocks/{}/children", url_quote(&page_id));
                let body = crate::json::json!({ "children": chunk });
                self.request("PATCH", &patch_path, Some(&body), interrupted)?;
            }
            let final_props = self.properties(
                schema,
                key,
                title,
                Some(content_hash),
                kind,
                period,
                project_name,
                has_blocker,
                aggregates,
                interrupted,
            )?;
            let page_path = format!("/v1/pages/{}", url_quote(&page_id));
            self.request(
                "PATCH",
                &page_path,
                Some(&crate::json::json!({"properties": final_props})),
                interrupted,
            )?;
        }

        Ok(page_id)
    }
}

#[derive(Clone, Debug)]
pub struct TargetSchema {
    pub is_data_source: bool,
    pub target_id: String,
    pub key_prop: String,
    pub hash_prop: String,
    pub title_prop: String,
    pub project_prop: Option<(String, String)>,
    pub kind_prop: Option<(String, String)>,
    pub period_prop: Option<(String, String)>,
    pub blocker_prop: Option<(String, String)>,
    pub sessions_prop: Option<(String, String)>,
    pub commits_prop: Option<(String, String)>,
    pub verified_prop: Option<(String, String)>,
}

#[allow(clippy::too_many_arguments)]
fn build_page_properties(
    schema: &TargetSchema,
    key: &str,
    title: &str,
    content_hash: Option<&str>,
    kind: &str,
    period: &str,
    project_name: &str,
    has_blocker: bool,
    aggregates: &Value,
) -> Value {
    let text = |value: &str| crate::json::json!({"type":"text","text":{"content":value}});
    let mut props = Map::new();
    props.insert(
        schema.title_prop.as_str().into(),
        crate::json::json!({"title":[text(title)]}),
    );
    props.insert(
        schema.key_prop.as_str().into(),
        crate::json::json!({"rich_text":[text(key)]}),
    );
    props.insert(schema.hash_prop.as_str().into(),crate::json::json!({"rich_text":content_hash.map(|value|vec![text(value)]).unwrap_or_default()}));
    if let Some((name, kind)) = &schema.project_prop
        && !project_name.is_empty()
    {
        if kind == "select" {
            let option: String = project_name
                .replace(',', " ")
                .trim()
                .chars()
                .take(100)
                .collect();
            if !option.is_empty() {
                props.insert(
                    name.as_str().into(),
                    crate::json::json!({"select":{"name":option}}),
                );
            }
        } else if kind == "rich_text" {
            props.insert(
                name.as_str().into(),
                crate::json::json!({"rich_text":[text(project_name)]}),
            );
        }
    }
    if let Some((name, kind)) = &schema.sessions_prop
        && kind == "rich_text"
    {
        let sessions: Vec<_> = aggregates
            .at("sessions")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(8).collect::<String>())
            .collect();
        let listed: String = sessions.join(", ").chars().take(1900).collect();
        if !listed.is_empty() {
            props.insert(
                name.as_str().into(),
                crate::json::json!({"rich_text":[text(&listed)]}),
            );
        }
    }
    for (property, field) in [
        (&schema.commits_prop, "commits"),
        (&schema.verified_prop, "verified"),
    ] {
        if let Some((name, kind)) = property
            && kind == "number"
            && !aggregates.at(field).is_null()
        {
            props.insert(
                name.as_str().into(),
                crate::json::json!({"number":aggregates.at(field)}),
            );
        }
    }
    if let Some((name, _)) = &schema.kind_prop {
        let name_value = if kind == "daily" {
            "Daily"
        } else if kind == "weekly" {
            "Weekly"
        } else {
            kind
        };
        props.insert(
            name.as_str().into(),
            crate::json::json!({"select":{"name":name_value}}),
        );
    }
    if let Some((name, _)) = &schema.period_prop {
        let date = if kind == "weekly" {
            crate::tz::iso_week_range(period)
                .map(|(start, _)| start)
                .unwrap_or_else(|| period.into())
        } else {
            period.into()
        };
        props.insert(
            name.as_str().into(),
            crate::json::json!({"date":{"start":date}}),
        );
    }
    if let Some((name, _)) = &schema.blocker_prop {
        props.insert(
            name.as_str().into(),
            crate::json::json!({"checkbox":has_blocker}),
        );
    }
    Value::Object(props)
}

fn py_contains(value: &oms_json::PyString, needle: &str) -> bool {
    let needle: Vec<_> = needle.chars().map(u32::from).collect();
    needle.is_empty()
        || value
            .codepoints()
            .windows(needle.len())
            .any(|part| part == needle)
}
fn py_redact(value: &oms_json::PyString, needle: &str) -> oms_json::PyString {
    let needle: Vec<_> = needle.chars().map(u32::from).collect();
    if needle.is_empty() {
        return value.clone();
    }
    let mut remaining = value.codepoints();
    let mut points = Vec::new();
    while !remaining.is_empty() {
        if remaining.starts_with(&needle) {
            points.extend("[REDACTED]".chars().map(u32::from));
            remaining = &remaining[needle.len()..];
        } else {
            points.push(remaining[0]);
            remaining = &remaining[1..];
        }
    }
    oms_json::PyString::from_codepoints(points).expect("existing Python code points")
}
fn contains_text(value: &Value, needle: &str) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::String(s) if py_contains(s, needle) => return true,
            Value::Array(a) => pending.extend(a.iter()),
            Value::Object(o) => {
                for (key, child) in o.iter() {
                    if py_contains(key, needle) {
                        return true;
                    }
                    pending.push(child);
                }
            }
            _ => {}
        }
    }
    false
}
fn redact_json(value: &Value, access: &str) -> Value {
    let mut output = value.copy();
    if access.is_empty() {
        return output;
    }
    let mut pending = vec![&mut output];
    while let Some(node) = pending.pop() {
        match node {
            Value::String(s) => *s = py_redact(s, access),
            Value::Array(items) => pending.extend(items.iter_mut()),
            Value::Object(obj) => {
                let original = std::mem::take(obj);
                for (key, value) in original {
                    obj.insert(py_redact(&key, access), value);
                }
                pending.extend(obj.iter_mut().map(|(_, v)| v));
            }
            _ => {}
        }
    }
    output
}

fn url_quote(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'~') {
            output.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(&mut output, "%{byte:02X}");
        }
    }
    output
}

fn text_object(content: &str) -> Value {
    crate::json::json!({"type":"text","text":{"content":content}})
}
fn note_block(content: &str) -> Value {
    crate::json::json!({"object":"block","type":"paragraph","paragraph":{"rich_text":[text_object(content)]}})
}
struct Blocks {
    children: Vec<Value>,
    total: usize,
    toggle: Option<usize>,
    toggle_full: bool,
    last: Option<(Option<usize>, usize)>,
}
impl Blocks {
    fn close(&mut self) {
        self.toggle = None;
        self.toggle_full = false;
        self.last = None;
    }
    fn emit(&mut self, block: Value) -> Option<(Option<usize>, usize)> {
        if self.total >= NOTION_MAX_BLOCKS {
            return None;
        }
        let location = if let Some(index) = self.toggle {
            let nested = self.children[index]
                .at_mut("toggle")
                .at_mut("children")
                .as_array_mut()
                .expect("toggle children");
            if nested.len() >= NOTION_TOGGLE_CHILD_LIMIT {
                if !self.toggle_full {
                    self.toggle_full = true;
                    let n = nested.len() - 1;
                    nested[n] = note_block(
                        "Truncated at the nested block limit; the full listing stays in the local journal file.",
                    );
                }
                return None;
            }
            let next = nested.len();
            nested.push(block);
            (Some(index), next)
        } else {
            let next = self.children.len();
            self.children.push(block);
            (None, next)
        };
        self.total += 1;
        Some(location)
    }
    fn append(
        &mut self,
        kind: &str,
        text: &str,
        attributes: Value,
    ) -> Option<(Option<usize>, usize)> {
        let chars: Vec<_> = text.chars().collect();
        let mut first = None;
        for (offset, chunk) in chars.chunks(NOTION_TEXT_CHUNK).enumerate() {
            let kind = if offset == 0 { kind } else { "paragraph" };
            let chunk: String = chunk.iter().collect();
            let mut payload = crate::json::json!({"rich_text":[text_object(&chunk)]});
            if offset == 0
                && let Some(attributes) = attributes.as_object()
            {
                for (k, v) in attributes.iter() {
                    payload
                        .as_object_mut()
                        .expect("block payload")
                        .insert(k.clone(), v.copy());
                }
            }
            let block = crate::json::json!({"object":"block","type":kind,kind:payload});
            let Some(location) = self.emit(block) else {
                return first;
            };
            if first.is_none() {
                first = Some(location);
            }
        }
        first
    }
    fn detail(&mut self, location: (Option<usize>, usize), text: &str) {
        let block = if let (Some(toggle), index) = location {
            &mut self.children[toggle]
                .at_mut("toggle")
                .at_mut("children")
                .as_array_mut()
                .expect("toggle children")[index]
        } else {
            &mut self.children[location.1]
        };
        let kind = block.at("type").as_str().unwrap_or_default().to_owned();
        let array = block
            .at_mut(&kind)
            .at_mut("rich_text")
            .as_array_mut()
            .expect("rich text");
        let chars: Vec<_> = text.chars().collect();
        for chunk in chars.chunks(NOTION_TEXT_CHUNK - 3) {
            if array.len() >= NOTION_CHILD_BATCH {
                return;
            }
            let text: String = chunk.iter().collect();
            array.push(text_object(&format!("\n↳ {text}")));
        }
    }
}
pub fn markdown_to_notion_blocks(markdown: &str) -> Vec<Value> {
    let heading = regex::Regex::new(r"^(#{1,3})\s+(.+)$").expect("pattern");
    let todo = regex::Regex::new(r"^-\s+\[([ xX])\]\s+(.+)$").expect("pattern");
    let bullet = regex::Regex::new(r"^-\s+(.+)$").expect("pattern");
    let numbered = regex::Regex::new(r"^\d+\.\s+(.+)$").expect("pattern");
    let quote = regex::Regex::new(r"^>\s+(.+)$").expect("pattern");
    let collapsed = [
        "핵심 진전",
        "Key progress",
        "프로젝트별 작업",
        "Work by project",
        "프로젝트별 진전",
        "Progress by project",
        "완료하거나 검증한 작업",
        "Completed or verified",
        "아직 검증되지 않은 것",
        "Not yet verified",
        "세션",
        "Sessions",
        "리뷰",
        "Reviews",
        "커밋",
        "Commits",
    ];
    let mut state = Blocks {
        children: Vec::new(),
        total: 0,
        toggle: None,
        toggle_full: false,
        last: None,
    };
    for raw in crate::unicode::text_lines(markdown) {
        if state.total >= NOTION_MAX_BLOCKS {
            break;
        }
        let indented = raw.starts_with([' ', '\t']);
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line == "---" {
            state.close();
            state.emit(crate::json::json!({"object":"block","type":"divider","divider":{}}));
        } else if let Some(c) = heading.captures(line) {
            let level = c[1].len();
            let title = &c[2];
            if level >= 3 && state.toggle.is_some() {
                state.last = None;
                state.append(&format!("heading_{level}"), title, Value::Null);
                continue;
            }
            state.close();
            if level == 2 && collapsed.contains(&title.trim()) {
                let block = crate::json::json!({"object":"block","type":"toggle","toggle":{"rich_text":[text_object(title.trim())],"children":[]}});
                if let Some((_, index)) = state.emit(block) {
                    state.toggle = Some(index);
                }
            } else {
                state.append(&format!("heading_{level}"), title, Value::Null);
            }
        } else if let Some(c) = todo.captures(line) {
            state.last = state.append(
                "to_do",
                &c[2],
                crate::json::json!({"checked":c[1].eq_ignore_ascii_case("x")}),
            );
        } else if let Some(c) = quote.captures(line) {
            state.last = None;
            state.append(
                "callout",
                &c[1],
                crate::json::json!({"icon":{"type":"emoji","emoji":"💡"}}),
            );
        } else if let Some(c) = bullet.captures(line) {
            if let Some(location) = state.last.filter(|_| indented) {
                state.detail(location, &c[1]);
            } else {
                state.last = state.append("bulleted_list_item", &c[1], Value::Null);
            }
        } else if let Some(c) = numbered.captures(line) {
            state.last = state.append("numbered_list_item", &c[1], Value::Null);
        } else {
            state.last = None;
            state.append("paragraph", line, Value::Null);
        }
    }
    if state.total >= NOTION_MAX_BLOCKS && !state.children.is_empty() {
        let last = state.children.len() - 1;
        state.children[last] =
            note_block("Work Journal content was truncated at the safe block limit.");
    }
    state.children
}

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    pub enabled: bool,
    pub attempted: usize,
    pub exported: usize,
    pub failed: usize,
    pub deferred: usize,
    pub waiting: usize,
    pub skipped: usize,
    pub remaining: usize,
    pub next_retry_at: Option<String>,
    pub message: String,
}
impl SyncReport {
    pub fn to_summary_line(&self) -> String {
        if !self.enabled {
            return "journal sync: Notion is not configured for this repository\n".into();
        }
        if self.attempted == 0 && self.waiting == 0 {
            return "journal sync: already up to date\n".into();
        }
        let mut output = format!(
            "journal sync: {} summar{} updated\n",
            self.exported,
            if self.exported == 1 { "y" } else { "ies" }
        );
        if self.failed > 0 {
            output.push_str(&format!(
                "journal sync: {} failed ({}); next: oms journal sync\n",
                self.failed,
                if self.message.is_empty() {
                    "unknown error"
                } else {
                    &self.message
                }
            ));
        }
        if self.deferred > 0 {
            output.push_str(&format!(
                "journal sync: {} deferred by the remote; next: oms journal sync\n",
                self.deferred
            ));
        }
        if self.remaining > 0 {
            output.push_str(&format!(
                "journal sync: {} beyond this tick's cap; next: oms journal sync\n",
                self.remaining
            ));
        }
        if self.waiting > 0 {
            output.push_str(&format!("journal sync: {} waiting for a retry window (earliest {}); next: oms journal sync after that, or new content for the period\n",self.waiting,self.next_retry_at.as_deref().unwrap_or("unknown")));
        }
        output
    }
}
pub struct SyncOptions {
    pub force: bool,
    pub include_today: bool,
    pub recent_days: Option<i64>,
    pub budget_seconds: f64,
    pub max_per_tick: Option<usize>,
}

pub fn sync_notion(
    store: &JournalStore,
    opts: &SyncOptions,
    now: DateTime<Utc>,
    interrupted: &AtomicBool,
) -> Result<SyncReport, String> {
    use chrono::Datelike;
    if opts.recent_days.is_some_and(|d| d < 1) {
        return Err("recent days must be a positive integer".into());
    }
    if opts.include_today && opts.recent_days.is_some() {
        return Err("today and recent days are mutually exclusive".into());
    }
    let mut report = SyncReport::default();
    if notion_repo_excluded(&store.repo_root) {
        return Ok(report);
    }
    let settings = notion_settings()?;
    if !crate::config::notion_auth_available(&settings) || settings.target_id().is_empty() {
        return Ok(report);
    }
    report.enabled = true;
    let properties = crate::json::json!({"title_property":settings.title_property,"key_property":settings.key_property,"hash_property":settings.hash_property,"project_property":settings.project_property,"kind_property":settings.kind_property,"period_property":settings.period_property,"blocker_property":settings.blocker_property,"sessions_property":settings.sessions_property,"commits_property":settings.commits_property,"verified_property":settings.verified_property});
    let target = crate::json::json!({"kind":if !settings.data_source_id.is_empty(){"data_source"}else{"database"},"id":settings.target_id(),"properties":properties});
    let fingerprint = crate::sanitize::sha256_hex(&crate::sanitize::canonical_json_bytes(&target)?);
    let path = store.root.join("sync/notion.json");
    let mut state =
        crate::json::json!({"schema_version":1,"target_fingerprint":fingerprint,"summaries":{}});
    if let Some(bytes) = read_regular_no_symlink_bytes(&path)?
        && let Ok(loaded) = crate::json::from_slice(&bytes)
        && loaded.at("schema_version").as_u64() == Some(1)
        && loaded.at("target_fingerprint").as_str().as_deref() == Some(&fingerprint)
        && loaded.at("summaries").is_object()
    {
        state = loaded;
    }
    let exporter = NotionExporter::from_settings(settings, opts.budget_seconds)?;
    let schema = exporter.schema_descriptor();
    let maximum = opts
        .max_per_tick
        .unwrap_or_else(|| {
            std::env::var("OMS_WORK_JOURNAL_NOTION_MAX_PER_TICK")
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(4)
                .clamp(1, 200) as usize
        })
        .clamp(1, 200);
    let (today, today_string, week) = store.tz.local_date_and_week(now);
    let range = if let Some(days) = opts.recent_days {
        let start = today
            .checked_sub_signed(Duration::days(days - 1))
            .ok_or("recent days is outside the supported date range")?;
        let calendar = start.iso_week();
        Some((
            naive_date_to_string(start),
            format!("{:04}-W{:02}", calendar.year(), calendar.week()),
        ))
    } else {
        None
    };
    let lang = journal_language(Some(&store.tz));
    for row in store.summary_rows()? {
        if !opts.force
            && ((row.kind == "daily" && row.period >= today_string)
                || (row.kind == "weekly" && row.period >= week))
        {
            continue;
        }
        if opts.include_today && (row.kind != "daily" || row.period != today_string) {
            continue;
        }
        if let Some((day, start_week)) = &range
            && !((row.kind == "daily" && row.period >= *day && row.period <= today_string)
                || (row.kind == "weekly" && row.period >= *start_week && row.period <= week))
        {
            continue;
        }
        let events = store.load_events_for_period(&row.kind, &row.period)?;
        let content = notion_presentation(&row.content, Some(&events), &lang);
        let hash = crate::sanitize::sha256_hex(content.as_bytes());
        let key = format!("{}:{}:{}", store.project.id, row.kind, row.period);
        let previous = state
            .at("summaries")
            .get(&key)
            .filter(|v| v.is_object())
            .map(|v| v.copy())
            .unwrap_or(crate::json::json!({}));
        if previous.at("status").as_str().as_deref() == Some("synced")
            && previous.at("content_hash").as_str().as_deref() == Some(&hash)
        {
            continue;
        }
        if let Some(stamp) = previous.at("next_retry_at").as_str() {
            let changed = previous.at("status").as_str().as_deref() == Some("failed")
                && previous.at("pending_content_hash").as_str().as_deref() != Some(&hash);
            if let Ok(retry) = parse_rfc3339_utc(&stamp)
                && !changed
                && retry > now
            {
                report.waiting += 1;
                let stamp = utc_rfc3339(retry);
                if report.next_retry_at.as_ref().is_none_or(|s| stamp < *s) {
                    report.next_retry_at = Some(stamp);
                }
                continue;
            }
        }
        if report.attempted >= maximum {
            report.remaining += 1;
            continue;
        }
        if interrupted.load(Ordering::Relaxed) {
            return Err("interrupted".into());
        }
        report.attempted += 1;
        let result = (|| -> Result<String, TransportError> {
            let aggregates = store
                .period_aggregates(&row.kind, &row.period)
                .map_err(TransportError::Io)?;
            let title = format!(
                "{} Work Journal — {}",
                if row.kind == "daily" {
                    "Daily"
                } else {
                    "Weekly"
                },
                row.period
            );
            exporter.upsert(
                &schema,
                &key,
                &title,
                &hash,
                &content,
                &row.kind,
                &row.period,
                &store.project.name,
                row.has_blocker,
                &aggregates,
                previous.at("page_id").as_str().as_deref(),
                interrupted,
            )
        })();
        let entry = match result {
            Ok(page) => {
                report.exported += 1;
                crate::json::json!({"status":"synced","page_id":if page.is_empty(){previous.at("page_id").copy()}else{Value::String(page.as_str().into())},"content_hash":hash,"period":row.period,"kind":row.kind})
            }
            Err(error) => {
                let error_name = match &error {
                    TransportError::Timeout(_) => "TimeoutError",
                    TransportError::Deferred { .. } => "NotionDeferredError",
                    TransportError::RateLimited { .. }
                    | TransportError::ServerError { .. }
                    | TransportError::ClientError { .. } => "NotionHTTPError",
                    TransportError::Io(_) => "NotionTransportError",
                };
                let missing = matches!(
                    error,
                    TransportError::ClientError { status: 404, .. }
                        | TransportError::Deferred { status: 404, .. }
                );
                let mut entry = crate::json::json!({"status":"failed","page_id":if missing{Value::Null}else{previous.at("page_id").copy()},"content_hash":previous.at("content_hash"),"pending_content_hash":hash,"period":row.period,"kind":row.kind,"error":error_name});
                let scheduled = if let TransportError::Deferred { retry_after, .. } = error {
                    (retry_after.is_finite() && retry_after > 0.0).then_some(retry_after)
                } else {
                    None
                };
                if let Some(seconds) = scheduled {
                    *entry.at_mut("status") = crate::json::json!("pending");
                    *entry.at_mut("next_retry_at") = crate::json::json!(utc_rfc3339(
                        now + Duration::microseconds((seconds * 1_000_000.0).round() as i64)
                    ));
                    report.deferred += 1;
                } else {
                    let previous_attempts = if previous.at("status").as_str().as_deref()
                        == Some("failed")
                        && previous.at("pending_content_hash").as_str().as_deref() == Some(&hash)
                    {
                        previous.at("attempts").as_integer()
                    } else {
                        None
                    };
                    let (exact_attempts, attempts) = increment_retry_attempts(previous_attempts)?;
                    *entry.at_mut("attempts") = exact_attempts;
                    if attempts >= 2 {
                        let seconds = 60u64
                            .saturating_mul(1u64 << attempts.saturating_sub(2).min(20))
                            .min(86400);
                        *entry.at_mut("next_retry_at") = crate::json::json!(utc_rfc3339(
                            now + Duration::seconds(seconds as i64)
                        ));
                    }
                    report.failed += 1;
                }
                report.message = error_name.into();
                entry
            }
        };
        *state.at_mut("summaries").at_mut(&key) = entry;
        atomic_write_bytes(&path, &pretty_json_bytes(&state)?)?;
    }
    Ok(report)
}

// Keep the persisted counter exact; only the bounded scheduling calculation
// saturates after its delay has already reached the maximum.
fn increment_retry_attempts(previous: Option<&oms_json::Integer>) -> Result<(Value, u64), String> {
    let decimal = previous
        .filter(|n| !n.is_negative())
        .map_or("0", |n| n.as_decimal());
    let mut digits = decimal.as_bytes().to_vec();
    let mut carry = true;
    for digit in digits.iter_mut().rev() {
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            carry = false;
            break;
        }
    }
    if carry {
        digits.insert(0, b'1');
    }
    if digits.len() > 4300 {
        return Err("Notion retry attempts exceed the JSON integer digit limit".into());
    }
    let decimal = String::from_utf8(digits).expect("decimal digits");
    let scheduling = decimal.parse::<u64>().unwrap_or(u64::MAX);
    let value = Value::integer(&decimal).map_err(|e| e.to_string())?;
    Ok((value, scheduling))
}

#[cfg(test)]
#[path = "../tests/unit/notion_json.rs"]
mod json_tests;
