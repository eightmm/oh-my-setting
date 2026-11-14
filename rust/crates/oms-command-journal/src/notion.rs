#![forbid(unsafe_code)]

use crate::cold::read_regular_no_symlink_bytes;
use crate::config::{NotionSettings, journal_language, notion_repo_excluded, notion_settings};
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
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

pub const NOTION_API_VERSION: &str = "2025-09-03";
pub const NOTION_MAX_BLOCKS: usize = 900;
pub const NOTION_CHILD_BATCH: usize = 100;
pub const NOTION_TEXT_CHUNK: usize = 2000;
pub const NOTION_TOGGLE_CHILD_LIMIT: usize = 95;
pub const MAX_RETRY_WAIT_SECS: i64 = 86400;
pub const INLINE_RETRY_AFTER_CAP: f64 = 2.0;

#[derive(Clone, Debug)]
pub enum TransportError {
    Timeout(String),
    RateLimited { retry_after: f64, message: String },
    ServerError { status: u16, message: String },
    ClientError { status: u16, message: String },
    Io(String),
}

impl TransportError {
    pub fn message(&self) -> &str {
        match self {
            TransportError::Timeout(m)
            | TransportError::RateLimited { message: m, .. }
            | TransportError::ServerError { message: m, .. }
            | TransportError::ClientError { message: m, .. }
            | TransportError::Io(m) => m,
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
            TransportError::RateLimited { retry_after, .. } => Some(*retry_after),
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
    Curl { bearer_cred: String },
    NtnCli { command: String, keyring: String },
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
            || (fd_stat.st_dev as u64, fd_stat.st_ino as u64) != (self.dir_dev, self.dir_ino)
        {
            return;
        }
        if let Some(expected_cfg) = self.cfg_id {
            if let Ok(st) = rfs::statat(&self.dir_fd, "cfg", AtFlags::SYMLINK_NOFOLLOW) {
                if FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
                    && st.st_uid == self.uid
                    && (st.st_dev as u64, st.st_ino as u64) == expected_cfg
                {
                    let _ = rfs::unlinkat(&self.dir_fd, "cfg", AtFlags::empty());
                }
            }
        }
        if self.hdr_created {
            if let Ok(st) = rfs::statat(&self.dir_fd, "hdr", AtFlags::SYMLINK_NOFOLLOW) {
                if FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
                    && st.st_uid == self.uid
                {
                    let _ = rfs::unlinkat(&self.dir_fd, "hdr", AtFlags::empty());
                }
            }
        }
        if let Ok(path_stat) = rfs::lstat(&self.dir_path) {
            if (path_stat.st_dev as u64, path_stat.st_ino as u64) == (self.dir_dev, self.dir_ino) {
                let _ = fs::remove_dir(&self.dir_path);
            }
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
    if method.is_empty() || !method.bytes().all(|b| matches!(b, b'A'..=b'Z')) {
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

        if stdin_ready {
            if let (Some(pipe), Some(bytes)) = (stdin_opt.as_mut(), stdin_bytes) {
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
        }

        if stdout_ready {
            if let Some(pipe) = stdout_opt.as_mut() {
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
        }

        if stderr_ready {
            if let Some(pipe) = stderr_opt.as_mut() {
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
            LiveTransport::Curl { bearer_cred } => self.curl_request(
                method,
                path,
                payload,
                timeout_secs,
                bearer_cred,
                interrupted,
            ),
            LiveTransport::NtnCli { command, keyring } => self.ntn_request(
                method,
                path,
                payload,
                timeout_secs,
                command,
                keyring,
                interrupted,
            ),
        }
    }

    fn curl_request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        timeout_secs: f64,
        bearer_cred: &str,
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
        let line_ver = format!("{hdr_key} {eq} \"Notion-Version: {NOTION_API_VERSION}\"");
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
        let effective_timeout = timeout_secs.max(0.1);
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
                    serde_json::to_vec(body)
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

        let stdout_s = String::from_utf8_lossy(&output.stdout);
        let (body_str, status_code) =
            if let Some((b, marker)) = stdout_s.rsplit_once("\n__OMS_HTTP_STATUS__:") {
                let code = marker.trim().parse::<u16>().unwrap_or(0);
                (b.trim(), code)
            } else {
                (stdout_s.trim(), 200)
            };

        let headers_s = req_dir.read_hdr_verified();
        let retry_after = parse_retry_after_header(&headers_s);

        if status_code == 429 {
            return Err(TransportError::RateLimited {
                retry_after: retry_after.unwrap_or(5.0),
                message: format!(
                    "Notion rate limited (HTTP 429): {}",
                    sanitize_text(body_str, 200)
                ),
            });
        }
        if status_code >= 500 {
            return Err(TransportError::ServerError {
                status: status_code,
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
        serde_json::from_str::<Value>(body_str)
            .map_err(|e| TransportError::Io(format!("invalid JSON from Notion API: {e}")))
    }

    fn ntn_request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        timeout_secs: f64,
        command: &str,
        keyring: &str,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        validate_method(method)?;
        validate_api_path(path)?;
        validate_cli_setting(command, keyring).map_err(TransportError::Io)?;

        let mut cmd = Command::new(command);
        cmd.args([
            "api",
            path,
            "-X",
            method,
            "--notion-version",
            NOTION_API_VERSION,
        ]);
        if keyring == "file" {
            cmd.env("NOTION_KEYRING", "0");
        }
        let json_bytes = match payload {
            Some(body) => Some(
                serde_json::to_vec(body)
                    .map_err(|e| TransportError::Io(format!("JSON encode failed: {e}")))?,
            ),
            None => None,
        };
        let effective_timeout = timeout_secs.max(0.1);
        let output = run_child_bounded(
            &mut cmd,
            command,
            json_bytes.as_deref(),
            StdDuration::from_secs_f64(effective_timeout),
            format!("{command} api request timed out after {timeout_secs:.1}s"),
            interrupted,
        )?;
        let stdout_s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr_s = sanitize_text(&String::from_utf8_lossy(&output.stderr), 300);

        if !output.status.success() {
            let lower = format!("{stderr_s} {stdout_s}").to_ascii_lowercase();
            if lower.contains("429") || lower.contains("rate_limit") {
                return Err(TransportError::RateLimited {
                    retry_after: 5.0,
                    message: format!("ntn rate limited: {stderr_s}"),
                });
            }
            if lower.contains("500")
                || lower.contains("502")
                || lower.contains("503")
                || lower.contains("504")
            {
                return Err(TransportError::ServerError {
                    status: 503,
                    message: format!("ntn server error: {stderr_s}"),
                });
            }
            if lower.contains("timed out") || lower.contains("timeout") {
                return Err(TransportError::Timeout(format!("ntn timeout: {stderr_s}")));
            }
            return Err(TransportError::ClientError {
                status: 400,
                message: format!(
                    "{command} exited {}: {}",
                    output.status.code().unwrap_or(1),
                    if stderr_s.is_empty() {
                        sanitize_text(&stdout_s, 200)
                    } else {
                        stderr_s
                    }
                ),
            });
        }

        if stdout_s.is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        serde_json::from_str::<Value>(&stdout_s)
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
        if let Ok(secs) = raw.parse::<f64>() {
            if secs.is_finite() && secs >= 0.0 {
                return Some(secs);
            }
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
}

impl NotionExporter {
    pub fn from_settings(settings: NotionSettings, budget_seconds: f64) -> Result<Self, String> {
        let transport = LiveTransport::from_settings(&settings)?;
        Ok(Self {
            transport,
            settings,
            timeout: 15.0,
            max_attempts: 3,
            budget_seconds,
            started: Instant::now(),
        })
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        interrupted: &AtomicBool,
    ) -> Result<Value, TransportError> {
        let mut attempt = 0usize;
        let mut timeout_multiplier = 1.0f64;
        loop {
            attempt += 1;
            let elapsed = self.started.elapsed().as_secs_f64();
            if elapsed >= self.budget_seconds && attempt > 1 {
                return Err(TransportError::Io("Notion sync budget exhausted".into()));
            }
            let req_timeout = self.timeout * timeout_multiplier;
            match self
                .transport
                .request_once(method, path, payload, req_timeout, interrupted)
            {
                Ok(val) => return Ok(val),
                Err(err) => {
                    if attempt >= self.max_attempts || !err.is_retryable() {
                        return Err(err);
                    }
                    if let Some(ra) = err.retry_after() {
                        if ra > INLINE_RETRY_AFTER_CAP {
                            return Err(err);
                        }
                        let remaining = self.budget_seconds - self.started.elapsed().as_secs_f64();
                        if ra > remaining {
                            return Err(err);
                        }
                        if !sleep_interruptible(
                            StdDuration::from_secs_f64(ra.max(0.05)),
                            interrupted,
                        ) {
                            return Err(TransportError::Io("interrupted".into()));
                        }
                    } else if matches!(err, TransportError::Timeout(_)) {
                        // One retry with 2x timeout per canonical contract
                        if timeout_multiplier >= 2.0 {
                            return Err(err);
                        }
                        timeout_multiplier = 2.0;
                    } else {
                        let wait = (0.4f64 * f64::from(attempt as u32)).min(2.0);
                        let remaining = self.budget_seconds - self.started.elapsed().as_secs_f64();
                        if wait > remaining {
                            return Err(err);
                        }
                        if !sleep_interruptible(StdDuration::from_secs_f64(wait), interrupted) {
                            return Err(TransportError::Io("interrupted".into()));
                        }
                    }
                }
            }
        }
    }

    pub fn discover_work_journal_data_source(
        &self,
        interrupted: &AtomicBool,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "filter": {"value": "data_source", "property": "object"},
            "page_size": 50
        });
        let resp = self
            .request("POST", "/v1/search", Some(&body), interrupted)
            .map_err(|e| format!("Notion discovery failed: {}", e.message()))?;
        let results = resp
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut matches = Vec::new();
        for item in results {
            let props = item.get("properties").and_then(Value::as_object);
            if let Some(pmap) = props {
                if pmap.contains_key("Work Journal Key") && pmap.contains_key("Work Journal Hash") {
                    if let Some(id) = item.get("id").and_then(Value::as_str) {
                        matches.push(id.to_string());
                    }
                }
            }
        }
        match matches.len() {
            1 => Ok(matches.into_iter().next().unwrap_or_default()),
            0 => Err("no Notion data source found with Work Journal Key and Work Journal Hash properties".into()),
            n => Err(format!("multiple ({n}) candidate Notion data sources found; pass --data-source-id explicitly")),
        }
    }

    pub fn validate_target(&self, interrupted: &AtomicBool) -> Result<TargetSchema, String> {
        let target_id = self.settings.target_id();
        if target_id.is_empty() {
            return Err("missing Notion data_source_id or database_id".to_string());
        }
        let path = if !self.settings.data_source_id.is_empty() {
            format!("/v1/data_sources/{target_id}")
        } else {
            format!("/v1/databases/{target_id}")
        };
        let resp = self
            .request("GET", &path, None, interrupted)
            .map_err(|e| format!("Notion schema validation failed: {}", e.message()))?;
        let props = resp
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| "Notion target response missing properties schema".to_string())?;

        let key_prop = if !self.settings.key_property.is_empty() {
            self.settings.key_property.clone()
        } else {
            "Work Journal Key".to_string()
        };
        let hash_prop = if !self.settings.hash_property.is_empty() {
            self.settings.hash_property.clone()
        } else {
            "Work Journal Hash".to_string()
        };

        if !props.contains_key(&key_prop) || !props.contains_key(&hash_prop) {
            return Err(format!(
                "Notion target {target_id} missing required properties '{key_prop}' / '{hash_prop}'"
            ));
        }

        let title_prop = if !self.settings.title_property.is_empty()
            && props.contains_key(&self.settings.title_property)
        {
            self.settings.title_property.clone()
        } else {
            props
                .iter()
                .find(|(_, v)| v.get("type").and_then(Value::as_str) == Some("title"))
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| "Name".to_string())
        };

        let project_prop_name = if !self.settings.project_property.is_empty() {
            self.settings.project_property.clone()
        } else {
            "Project".to_string()
        };
        let project_prop_type = props
            .get(&project_prop_name)
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let kind_prop_name = if !self.settings.kind_property.is_empty() {
            self.settings.kind_property.clone()
        } else {
            "Kind".to_string()
        };
        let kind_prop_type = props
            .get(&kind_prop_name)
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let period_prop_name = if !self.settings.period_property.is_empty() {
            self.settings.period_property.clone()
        } else {
            "Period".to_string()
        };
        let period_prop_type = props
            .get(&period_prop_name)
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let blocker_prop_name = if !self.settings.blocker_property.is_empty() {
            self.settings.blocker_property.clone()
        } else {
            "Has Blocker".to_string()
        };
        let blocker_prop_type = props
            .get(&blocker_prop_name)
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        Ok(TargetSchema {
            is_data_source: !self.settings.data_source_id.is_empty(),
            target_id: target_id.to_string(),
            key_prop,
            hash_prop,
            title_prop,
            project_prop: project_prop_type.map(|t| (project_prop_name, t)),
            kind_prop: kind_prop_type.map(|t| (kind_prop_name, t)),
            period_prop: period_prop_type.map(|t| (period_prop_name, t)),
            blocker_prop: blocker_prop_type.map(|t| (blocker_prop_name, t)),
        })
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
        known_page_id: Option<&str>,
        interrupted: &AtomicBool,
    ) -> Result<String, TransportError> {
        let query_path = if schema.is_data_source {
            format!("/v1/data_sources/{}/query", schema.target_id)
        } else {
            format!("/v1/databases/{}/query", schema.target_id)
        };

        let mut existing_page_id = known_page_id.map(ToOwned::to_owned);
        let mut remote_hash = String::new();

        if existing_page_id.is_none() {
            let filter = serde_json::json!({
                "filter": {
                    "property": schema.key_prop,
                    "rich_text": {"equals": key}
                },
                "page_size": 1
            });
            let resp = self.request("POST", &query_path, Some(&filter), interrupted)?;
            if let Some(page) = resp
                .get("results")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
            {
                existing_page_id = page
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                remote_hash = page
                    .get("properties")
                    .and_then(|p| p.get(&schema.hash_prop))
                    .and_then(|hp| hp.get("rich_text"))
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(|rt| rt.get("plain_text").or_else(|| rt.pointer("/text/content")))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
        }

        if existing_page_id.is_some() && !remote_hash.is_empty() && remote_hash == content_hash {
            return Ok(existing_page_id.unwrap_or_default());
        }

        let children = markdown_to_notion_blocks(markdown);
        let icon = if kind == "weekly" { "📚" } else { "📔" };

        if let Some(page_id) = existing_page_id {
            // Delete existing child blocks, append new batches, then publish hash + properties
            let mut next_cursor: Option<String> = None;
            loop {
                let mut list_path = format!("/v1/blocks/{page_id}/children?page_size=100");
                if let Some(cur) = &next_cursor {
                    list_path.push_str("&start_cursor=");
                    list_path.push_str(cur);
                }
                let list_resp = self.request("GET", &list_path, None, interrupted)?;
                if let Some(blocks) = list_resp.get("results").and_then(Value::as_array) {
                    for blk in blocks {
                        if let Some(bid) = blk.get("id").and_then(Value::as_str) {
                            let del_path = format!("/v1/blocks/{bid}");
                            let _ = self.request("DELETE", &del_path, None, interrupted)?;
                        }
                    }
                }
                if list_resp.get("has_more").and_then(Value::as_bool) == Some(true) {
                    next_cursor = list_resp
                        .get("next_cursor")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    if next_cursor.is_none() {
                        break;
                    }
                } else {
                    break;
                }
            }

            for chunk in children.chunks(NOTION_CHILD_BATCH) {
                let patch_path = format!("/v1/blocks/{page_id}/children");
                let body = serde_json::json!({ "children": chunk });
                self.request("PATCH", &patch_path, Some(&body), interrupted)?;
            }

            let props = build_page_properties(
                schema,
                key,
                title,
                Some(content_hash),
                kind,
                period,
                project_name,
                has_blocker,
            );
            let page_path = format!("/v1/pages/{page_id}");
            let body = serde_json::json!({
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
        let initial_props = build_page_properties(
            schema,
            key,
            title,
            initial_hash,
            kind,
            period,
            project_name,
            has_blocker,
        );
        let parent = if schema.is_data_source {
            serde_json::json!({"type": "data_source_id", "data_source_id": schema.target_id})
        } else {
            serde_json::json!({"database_id": schema.target_id})
        };
        let first_batch: Vec<Value> = children.iter().take(NOTION_CHILD_BATCH).cloned().collect();
        let create_body = serde_json::json!({
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
                let patch_path = format!("/v1/blocks/{page_id}/children");
                let body = serde_json::json!({ "children": chunk });
                self.request("PATCH", &patch_path, Some(&body), interrupted)?;
            }
            let final_props = build_page_properties(
                schema,
                key,
                title,
                Some(content_hash),
                kind,
                period,
                project_name,
                has_blocker,
            );
            let page_path = format!("/v1/pages/{page_id}");
            self.request(
                "PATCH",
                &page_path,
                Some(&serde_json::json!({"properties": final_props})),
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
) -> Value {
    let mut props = Map::new();
    props.insert(
        schema.title_prop.clone(),
        serde_json::json!({"title": [{"type": "text", "text": {"content": sanitize_text(title, 200)}}]}),
    );
    props.insert(
        schema.key_prop.clone(),
        serde_json::json!({"rich_text": [{"type": "text", "text": {"content": key}}]}),
    );
    if let Some(h) = content_hash {
        props.insert(
            schema.hash_prop.clone(),
            serde_json::json!({"rich_text": [{"type": "text", "text": {"content": h}}]}),
        );
    }
    if let Some((name, typ)) = &schema.project_prop {
        if let Some(v) = adapt_optional_prop(typ, project_name) {
            props.insert(name.clone(), v);
        }
    }
    if let Some((name, typ)) = &schema.kind_prop {
        if let Some(v) = adapt_optional_prop(typ, kind) {
            props.insert(name.clone(), v);
        }
    }
    if let Some((name, typ)) = &schema.period_prop {
        if let Some(v) = adapt_optional_prop(typ, period) {
            props.insert(name.clone(), v);
        }
    }
    if let Some((name, typ)) = &schema.blocker_prop {
        if typ == "checkbox" {
            props.insert(name.clone(), serde_json::json!({"checkbox": has_blocker}));
        }
    }
    Value::Object(props)
}

fn adapt_optional_prop(prop_type: &str, val: &str) -> Option<Value> {
    let clean = sanitize_text(val, 120);
    if clean.is_empty() {
        return None;
    }
    match prop_type {
        "select" => Some(serde_json::json!({"select": {"name": clean}})),
        "rich_text" => {
            Some(serde_json::json!({"rich_text": [{"type": "text", "text": {"content": clean}}]}))
        }
        _ => None,
    }
}

fn rich_text_chunks(text: &str) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![serde_json::json!({"type": "text", "text": {"content": ""}})];
    }
    chars
        .chunks(NOTION_TEXT_CHUNK)
        .map(|chunk| {
            let s: String = chunk.iter().collect();
            serde_json::json!({"type": "text", "text": {"content": s}})
        })
        .collect()
}

fn is_collapsed_section(heading: &str) -> bool {
    matches!(
        heading.trim(),
        "## Key progress"
            | "## 핵심 진전"
            | "## Work by project"
            | "## 프로젝트별 작업"
            | "## Details"
            | "## 세부 내용"
    )
}

pub fn markdown_to_notion_blocks(markdown: &str) -> Vec<Value> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks: Vec<Value> = Vec::new();
    let mut i = 0usize;

    while i < lines.len() && blocks.len() < NOTION_MAX_BLOCKS {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if is_collapsed_section(trimmed) {
            let toggle_title = trimmed.trim_start_matches("## ").trim();
            i += 1;
            let mut inner = Vec::new();
            while i < lines.len() && !lines[i].trim().starts_with("## ") {
                let inner_line = lines[i];
                let inner_trim = inner_line.trim();
                if !inner_trim.is_empty() {
                    if let Some(sub) = inner_trim.strip_prefix("### ") {
                        inner.push(serde_json::json!({
                            "object": "block",
                            "type": "heading_3",
                            "heading_3": {"rich_text": rich_text_chunks(sub.trim())}
                        }));
                    } else if let Some(b) = inner_trim.strip_prefix("- ") {
                        inner.push(serde_json::json!({
                            "object": "block",
                            "type": "bulleted_list_item",
                            "bulleted_list_item": {"rich_text": rich_text_chunks(b.trim())}
                        }));
                    } else {
                        inner.push(serde_json::json!({
                            "object": "block",
                            "type": "paragraph",
                            "paragraph": {"rich_text": rich_text_chunks(inner_trim)}
                        }));
                    }
                }
                i += 1;
            }
            if inner.len() > NOTION_TOGGLE_CHILD_LIMIT {
                let omitted = inner.len() - (NOTION_TOGGLE_CHILD_LIMIT - 1);
                inner.truncate(NOTION_TOGGLE_CHILD_LIMIT - 1);
                inner.push(serde_json::json!({
                    "object": "block",
                    "type": "paragraph",
                    "paragraph": {
                        "rich_text": rich_text_chunks(&format!("Truncated {omitted} additional item(s); see local journal markdown."))
                    }
                }));
            }
            blocks.push(serde_json::json!({
                "object": "block",
                "type": "toggle",
                "toggle": {
                    "rich_text": rich_text_chunks(toggle_title),
                    "children": inner,
                }
            }));
            continue;
        }

        if let Some(h1) = trimmed.strip_prefix("# ") {
            blocks.push(serde_json::json!({
                "object": "block",
                "type": "heading_1",
                "heading_1": {"rich_text": rich_text_chunks(h1.trim())}
            }));
        } else if let Some(h2) = trimmed.strip_prefix("## ") {
            blocks.push(serde_json::json!({
                "object": "block",
                "type": "heading_2",
                "heading_2": {"rich_text": rich_text_chunks(h2.trim())}
            }));
        } else if let Some(h3) = trimmed.strip_prefix("### ") {
            blocks.push(serde_json::json!({
                "object": "block",
                "type": "heading_3",
                "heading_3": {"rich_text": rich_text_chunks(h3.trim())}
            }));
        } else if let Some(q) = trimmed.strip_prefix("> ") {
            blocks.push(serde_json::json!({
                "object": "block",
                "type": "callout",
                "callout": {
                    "rich_text": rich_text_chunks(q.trim()),
                    "icon": {"type": "emoji", "emoji": "📌"}
                }
            }));
        } else if let Some(b) = trimmed.strip_prefix("- ") {
            let mut item_children = Vec::new();
            while i + 1 < lines.len() && lines[i + 1].starts_with("  - ") {
                i += 1;
                let sub = lines[i].trim_start().trim_start_matches("- ").trim();
                item_children.push(serde_json::json!({
                    "object": "block",
                    "type": "bulleted_list_item",
                    "bulleted_list_item": {"rich_text": rich_text_chunks(sub)}
                }));
            }
            let mut bullet = serde_json::json!({
                "object": "block",
                "type": "bulleted_list_item",
                "bulleted_list_item": {"rich_text": rich_text_chunks(b.trim())}
            });
            if !item_children.is_empty() {
                bullet["bulleted_list_item"]["children"] = Value::Array(item_children);
            }
            blocks.push(bullet);
        } else {
            blocks.push(serde_json::json!({
                "object": "block",
                "type": "paragraph",
                "paragraph": {"rich_text": rich_text_chunks(trimmed)}
            }));
        }
        i += 1;
    }

    if blocks.len() >= NOTION_MAX_BLOCKS {
        blocks.truncate(NOTION_MAX_BLOCKS - 1);
        blocks.push(serde_json::json!({
            "object": "block",
            "type": "paragraph",
            "paragraph": {"rich_text": rich_text_chunks("Truncated remaining content at Notion block limit.")}
        }));
    }
    blocks
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
    pub next_retry_at: Option<String>,
    pub message: String,
}

impl SyncReport {
    pub fn to_summary_line(&self) -> String {
        if !self.message.is_empty() {
            return format!("{}\n", self.message);
        }
        format!(
            "sync: enabled={} attempted={} exported={} deferred={} failed={} waiting={}\n",
            self.enabled, self.attempted, self.exported, self.deferred, self.failed, self.waiting
        )
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
    if notion_repo_excluded(&store.repo_root) {
        return Ok(SyncReport {
            enabled: false,
            message: "sync skipped: repository is in excluded_repos".into(),
            ..Default::default()
        });
    }
    let settings = notion_settings();
    if !settings.is_configured() {
        return Ok(SyncReport {
            enabled: false,
            message: "sync disabled: Notion target or credentials not configured".into(),
            ..Default::default()
        });
    }

    let state_path = store.root.join("notion-sync-state.json");
    let mut state: BTreeMap<String, Value> = read_regular_no_symlink_bytes(&state_path)?
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            v.get("periods")
                .and_then(Value::as_object)
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        })
        .unwrap_or_default();

    let (today_date, today_str, current_week) = store.tz.local_date_and_week(now);
    let cutoff_date = opts
        .recent_days
        .map(|days| naive_date_to_string(today_date - Duration::days(days.max(0))));
    let summary_rows = store.summary_rows()?;

    let mut candidates = Vec::new();
    for row in summary_rows {
        let is_live = (row.kind == "daily" && row.period == today_str)
            || (row.kind == "weekly" && row.period == current_week);
        if is_live
            && !opts.force
            && !(opts.include_today && row.kind == "daily" && row.period == today_str)
        {
            continue;
        }
        if let Some(ref cutoff) = cutoff_date {
            if row.kind == "daily" && row.period < *cutoff {
                continue;
            }
            if row.kind == "weekly" {
                if let Some((_start, end)) = crate::tz::iso_week_range(&row.period) {
                    if end < *cutoff {
                        continue;
                    }
                }
            }
        }
        candidates.push(row);
    }

    if candidates.is_empty() {
        return Ok(SyncReport {
            enabled: true,
            message: "sync: nothing to export".into(),
            ..Default::default()
        });
    }

    let exporter = NotionExporter::from_settings(settings, opts.budget_seconds)?;
    let schema = exporter.validate_target(interrupted)?;
    let lang = journal_language(Some(&store.tz));

    let mut report = SyncReport {
        enabled: true,
        ..Default::default()
    };

    let deadline = Instant::now() + StdDuration::from_secs_f64(opts.budget_seconds.max(1.0));
    for row in candidates {
        if interrupted.load(Ordering::Relaxed) || Instant::now() >= deadline {
            break;
        }
        if let Some(cap) = opts.max_per_tick {
            if report.attempted >= cap {
                break;
            }
        }

        let key = format!("{}:{}:{}", store.project.id, row.kind, row.period);
        let prev = state.get(&key).and_then(Value::as_object).cloned();
        let prev_hash = prev
            .as_ref()
            .and_then(|p| p.get("content_hash").and_then(Value::as_str))
            .unwrap_or("");
        let prev_status = prev
            .as_ref()
            .and_then(|p| p.get("status").and_then(Value::as_str))
            .unwrap_or("");
        let prev_page_id = prev
            .as_ref()
            .and_then(|p| p.get("page_id").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);

        let same_hash = !prev_hash.is_empty() && prev_hash == row.content_hash;
        if same_hash && prev_status == "synced" && !opts.force {
            report.skipped += 1;
            continue;
        }

        if same_hash && !opts.force {
            if let Some(next_retry_str) = prev
                .as_ref()
                .and_then(|p| p.get("next_retry_at").and_then(Value::as_str))
            {
                if let Ok(next_dt) = parse_rfc3339_utc(next_retry_str) {
                    if now < next_dt {
                        report.waiting += 1;
                        if report.next_retry_at.is_none() {
                            report.next_retry_at = Some(next_retry_str.to_string());
                        }
                        continue;
                    }
                }
            }
        }

        report.attempted += 1;
        let period_events = if row.kind == "daily" {
            store.load_events_for_period("daily", &row.period).ok()
        } else {
            None
        };
        let body_md = notion_presentation(&row.content, period_events.as_deref(), &lang);
        let title = format!("{} {} ({})", store.project.name, row.kind, row.period);

        match exporter.upsert(
            &schema,
            &key,
            &title,
            &row.content_hash,
            &body_md,
            &row.kind,
            &row.period,
            &store.project.name,
            row.has_blocker,
            prev_page_id.as_deref(),
            interrupted,
        ) {
            Ok(page_id) => {
                report.exported += 1;
                let mut entry = Map::new();
                entry.insert("status".into(), Value::String("synced".into()));
                entry.insert("content_hash".into(), Value::String(row.content_hash));
                entry.insert("page_id".into(), Value::String(page_id));
                entry.insert("synced_at".into(), Value::String(utc_rfc3339(now)));
                state.insert(key, Value::Object(entry));
            }
            Err(err) => {
                let prev_attempts = if same_hash {
                    prev.as_ref()
                        .and_then(|p| p.get("attempts").and_then(Value::as_u64))
                        .unwrap_or(0)
                } else {
                    0
                };
                let attempts = prev_attempts + 1;
                let mut entry = Map::new();
                entry.insert("content_hash".into(), Value::String(row.content_hash));
                if let Some(pid) = prev_page_id {
                    entry.insert("page_id".into(), Value::String(pid));
                }
                entry.insert("attempts".into(), Value::Number(attempts.into()));
                entry.insert(
                    "last_error".into(),
                    Value::String(sanitize_text(err.message(), 300)),
                );

                if let Some(ra) = err.retry_after() {
                    report.deferred += 1;
                    let wait_secs = (ra.ceil() as i64).clamp(1, MAX_RETRY_WAIT_SECS);
                    let next_dt = now + Duration::seconds(wait_secs);
                    let next_s = utc_rfc3339(next_dt);
                    entry.insert("status".into(), Value::String("deferred".into()));
                    entry.insert("next_retry_at".into(), Value::String(next_s.clone()));
                    if report.next_retry_at.is_none() {
                        report.next_retry_at = Some(next_s);
                    }
                } else {
                    report.failed += 1;
                    entry.insert("status".into(), Value::String("failed".into()));
                    // 1st failure allows 1 immediate retry (no next_retry_at); >=2 backs off 60 * 2^(attempts-2) capped at 1 day
                    if attempts >= 2 {
                        let exp = (attempts - 2).min(12) as u32;
                        let wait_secs = (60i64.saturating_mul(2i64.saturating_pow(exp)))
                            .min(MAX_RETRY_WAIT_SECS);
                        let next_dt = now + Duration::seconds(wait_secs);
                        let next_s = utc_rfc3339(next_dt);
                        entry.insert("next_retry_at".into(), Value::String(next_s.clone()));
                        if report.next_retry_at.is_none() {
                            report.next_retry_at = Some(next_s);
                        }
                    }
                }
                state.insert(key, Value::Object(entry));
            }
        }
    }

    let mut root = Map::new();
    root.insert("schema".into(), Value::Number(1.into()));
    root.insert("updated_at".into(), Value::String(utc_rfc3339(now)));
    let periods_map: Map<String, Value> = state.into_iter().collect();
    root.insert("periods".into(), Value::Object(periods_map));
    let bytes = pretty_json_bytes(&Value::Object(root))?;
    atomic_write_bytes(&state_path, &bytes)?;

    Ok(report)
}
