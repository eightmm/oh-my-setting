//! Bounded `curl` subprocess HTTP JSON client keeping credentials out of `argv`.

use crate::crypto::SecretValue;
use crate::source::TransportError;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus, geteuid, kill_process_group, waitid,
};
use serde_json::Value;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CHILD_STDOUT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHILD_STDERR_BYTES: u64 = 256 * 1024;

static REQUEST_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointScheme {
    Https,
    LoopbackHttp,
}

#[derive(Clone, Debug)]
pub struct EndpointUrl {
    url: String,
    scheme: EndpointScheme,
}

impl EndpointUrl {
    pub fn from_env(var_name: &str, default_url: &str) -> Result<Self, String> {
        match std::env::var_os(var_name) {
            None => Self::parse(default_url),
            Some(raw) if raw.is_empty() => Self::parse(default_url),
            Some(raw) => {
                let text = raw
                    .into_string()
                    .map_err(|_| format!("invalid {var_name}: must be valid UTF-8"))?;
                Self::parse(&text)
            }
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty()
            || raw.len() > 2048
            || raw.starts_with('-')
            || raw.bytes().any(|b| b <= 0x20 || b == 0x7f)
        {
            return Err("invalid intake API URL".into());
        }
        if let Some(rest) = raw.strip_prefix("https://") {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            if authority.is_empty() || authority.contains('@') {
                return Err("invalid https intake API URL".into());
            }
            return Ok(Self {
                url: raw.to_owned(),
                scheme: EndpointScheme::Https,
            });
        }
        if let Some(rest) = raw.strip_prefix("http://") {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            if authority.is_empty() || authority.contains('@') || !is_loopback_authority(authority)
            {
                return Err(
                    "intake API URL over http is accepted only for loopback addresses".into(),
                );
            }
            return Ok(Self {
                url: raw.to_owned(),
                scheme: EndpointScheme::LoopbackHttp,
            });
        }
        Err("intake API URL must use https or loopback http".into())
    }

    pub fn as_str(&self) -> &str {
        &self.url
    }

    pub(crate) fn curl_proto(&self) -> &'static str {
        match self.scheme {
            EndpointScheme::Https => "=https",
            EndpointScheme::LoopbackHttp => "=http",
        }
    }
}

fn is_loopback_authority(authority: &str) -> bool {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((ip_part, rest)) = bracketed.split_once(']') else {
            return false;
        };
        if !rest.is_empty() && !rest.strip_prefix(':').is_some_and(valid_port) {
            return false;
        }
        return ip_part.parse::<Ipv6Addr>().is_ok_and(|ip| ip.is_loopback());
    }
    let (host, port_opt) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if let Some(p) = port_opt
        && !valid_port(p)
    {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

fn valid_port(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|p| p > 0)
}

struct RequestTempDir {
    dir_path: PathBuf,
    dir_fd: OwnedFd,
    dir_dev: u64,
    dir_ino: u64,
    uid: u32,
    cfg_id: Option<(u64, u64)>,
}

impl RequestTempDir {
    #[allow(clippy::unnecessary_cast)]
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
            let dir_path = tmp_base.join(format!("oms-intake-curl-{pid}-{seq}-{tag:016x}"));

            match rfs::mkdir(&dir_path, Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let dir_fd = rfs::open(
                        &dir_path,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|e| {
                        TransportError::OutcomeUnknown(format!(
                            "failed opening private request temp dir: {e}"
                        ))
                    })?;
                    let fd_stat = rfs::fstat(&dir_fd).map_err(|e| {
                        TransportError::OutcomeUnknown(format!(
                            "failed inspecting private request temp dir: {e}"
                        ))
                    })?;
                    let path_stat = rfs::lstat(&dir_path).map_err(|e| {
                        TransportError::OutcomeUnknown(format!(
                            "failed verifying private request temp dir: {e}"
                        ))
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
                        return Err(TransportError::OutcomeUnknown(
                            "private request temp dir failed identity verification".into(),
                        ));
                    }
                    return Ok(Self {
                        dir_path,
                        dir_fd,
                        dir_dev: fd_stat.st_dev as u64,
                        dir_ino: fd_stat.st_ino as u64,
                        uid,
                        cfg_id: None,
                    });
                }
                Err(Errno::EXIST) => continue,
                Err(e) => {
                    return Err(TransportError::OutcomeUnknown(format!(
                        "failed creating private request temp dir: {e}"
                    )));
                }
            }
        }
        Err(TransportError::OutcomeUnknown(
            "exhausted attempts creating private request temp dir".into(),
        ))
    }

    fn cfg_path(&self) -> PathBuf {
        self.dir_path.join("cfg")
    }

    #[allow(clippy::unnecessary_cast)]
    fn write_cfg_exclusive(&mut self, body: &mut [u8]) -> Result<(), TransportError> {
        let fd = rfs::openat(
            &self.dir_fd,
            "cfg",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| TransportError::OutcomeUnknown(format!("failed creating curl config: {e}")))?;
        let st = rfs::fstat(&fd).map_err(|e| {
            TransportError::OutcomeUnknown(format!("failed inspecting curl config: {e}"))
        })?;
        self.cfg_id = Some((st.st_dev as u64, st.st_ino as u64));

        let ftype = FileType::from_raw_mode(st.st_mode);
        let mode_bits = st.st_mode & 0o777;
        if ftype != FileType::RegularFile
            || st.st_uid != self.uid
            || (mode_bits & 0o077) != 0
            || (mode_bits & 0o600) != 0o600
            || st.st_nlink != 1
        {
            body.fill(0);
            black_box(&body);
            return Err(TransportError::OutcomeUnknown(
                "curl config failed exclusive regular-file verification".into(),
            ));
        }

        let mut file = File::from(fd);
        let write_res = file.write_all(body).and_then(|()| file.flush());
        body.fill(0);
        black_box(&body);
        write_res
            .map_err(|e| TransportError::OutcomeUnknown(format!("failed writing curl config: {e}")))
    }
}

impl Drop for RequestTempDir {
    #[allow(clippy::unnecessary_cast)]
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
        if let Some(expected_cfg) = self.cfg_id
            && let Ok(st) = rfs::statat(&self.dir_fd, "cfg", AtFlags::SYMLINK_NOFOLLOW)
            && FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
            && st.st_uid == self.uid
            && (st.st_dev as u64, st.st_ino as u64) == expected_cfg
        {
            let _ = rfs::unlinkat(&self.dir_fd, "cfg", AtFlags::empty());
        }
        if let Ok(path_stat) = rfs::lstat(&self.dir_path)
            && (path_stat.st_dev as u64, path_stat.st_ino as u64) == (self.dir_dev, self.dir_ino)
        {
            let _ = fs::remove_dir(&self.dir_path);
        }
    }
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

    fn probe_exited_nowait(&mut self) -> Result<Option<WaitIdStatus>, TransportError> {
        let Some(pgid) = self.pgid else {
            self.identity_uncertain = true;
            return Err(TransportError::OutcomeUnknown(
                "child process missing valid pid/pgid".into(),
            ));
        };
        loop {
            match waitid(
                WaitId::Pid(pgid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(status) => {
                    return Ok(status.filter(|s| s.exited() || s.killed() || s.dumped()));
                }
                Err(Errno::INTR) => continue,
                Err(Errno::CHILD) => {
                    self.identity_uncertain = true;
                    self.reaped = true;
                    return Err(TransportError::OutcomeUnknown(
                        "child process identity lost (ECHILD)".into(),
                    ));
                }
                Err(e) => {
                    return Err(TransportError::OutcomeUnknown(format!(
                        "waitid on child process failed: {e}"
                    )));
                }
            }
        }
    }

    fn reap_after_group_kill(&mut self) -> Result<ExitStatus, TransportError> {
        if self.reaped || self.identity_uncertain {
            return Err(TransportError::OutcomeUnknown(
                "cannot reap child: already reaped or identity uncertain".into(),
            ));
        }
        if let Some(pgid) = self.pgid {
            let _ = kill_process_group(pgid, Signal::KILL);
        }
        self.reaped = true;
        self.child.wait().map_err(|e| {
            TransportError::OutcomeUnknown(format!("failed reaping child process: {e}"))
        })
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

fn sanitize_diag(raw: &[u8], cred: &str) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let redacted = if !cred.is_empty() {
        lossy.replace(cred, "[REDACTED]")
    } else {
        lossy.into_owned()
    };
    let mut out = String::new();
    for ch in redacted.chars() {
        let mapped = if ch.is_control() { ' ' } else { ch };
        if out.len() + mapped.len_utf8() > 200 {
            break;
        }
        out.push(mapped);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn drain_pipe(
    pipe: &mut impl std::io::Read,
    buf: &mut Vec<u8>,
    limit: u64,
    truncated: &mut bool,
) -> Result<PipeDrain, TransportError> {
    let mut scratch = [0u8; 8192];
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
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(PipeDrain::Open),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(TransportError::OutcomeUnknown(format!(
                    "failed reading curl pipe: {e}"
                )));
            }
        }
    }
    Ok(PipeDrain::Open)
}

fn run_curl_bounded(
    cmd: &mut Command,
    stdin_bytes: &[u8],
    timeout: Duration,
    interrupted: &AtomicBool,
    cred_for_redaction: &str,
) -> Result<ChildRunOutput, TransportError> {
    if interrupted.load(Ordering::Relaxed) {
        return Err(TransportError::OutcomeUnknown("interrupted".into()));
    }
    cmd.process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| TransportError::OutcomeUnknown(format!("failed spawning curl: {e}")))?;
    let mut guard = ChildGroupGuard::new(child);

    let mut stdin_opt = match guard.child.stdin.take() {
        Some(pipe) if !stdin_bytes.is_empty() => {
            set_nonblocking(&pipe).map_err(|e| {
                TransportError::OutcomeUnknown(format!("failed configuring curl stdin: {e}"))
            })?;
            Some(pipe)
        }
        Some(pipe) => {
            drop(pipe);
            None
        }
        None => {
            return Err(TransportError::OutcomeUnknown(
                "missing piped stdin handle for curl".into(),
            ));
        }
    };

    let stdout_pipe = guard.child.stdout.take().ok_or_else(|| {
        TransportError::OutcomeUnknown("missing piped stdout handle for curl".into())
    })?;
    set_nonblocking(&stdout_pipe).map_err(|e| {
        TransportError::OutcomeUnknown(format!("failed configuring curl stdout: {e}"))
    })?;
    let mut stdout_opt = Some(stdout_pipe);

    let stderr_pipe = guard.child.stderr.take().ok_or_else(|| {
        TransportError::OutcomeUnknown("missing piped stderr handle for curl".into())
    })?;
    set_nonblocking(&stderr_pipe).map_err(|e| {
        TransportError::OutcomeUnknown(format!("failed configuring curl stderr: {e}"))
    })?;
    let mut stderr_opt = Some(stderr_pipe);

    let mut stdin_offset = 0usize;
    let mut stdin_broken_pipe = false;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let mut stdout_truncated = false;
    let mut stderr_truncated = false;
    let mut leader_exited = false;
    let mut wait_step_ms = 1u64;
    let deadline = Instant::now() + timeout;

    loop {
        if interrupted.load(Ordering::Relaxed) {
            return Err(TransportError::OutcomeUnknown("interrupted".into()));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(TransportError::OutcomeUnknown(
                "external API request timed out".into(),
            ));
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
            thread::sleep(remaining.min(Duration::from_millis(wait_step_ms)));
            wait_step_ms = (wait_step_ms * 2).min(5);
            continue;
        }

        let slice = remaining.min(Duration::from_millis(15));
        let ts = Timespec::try_from(slice)
            .map_err(|e| TransportError::OutcomeUnknown(format!("invalid poll timeout: {e}")))?;

        let (si_r, so_r, se_r, bad_fd) = {
            let mut fds = Vec::with_capacity(3);
            let si_idx = stdin_opt.as_ref().map(|fd| {
                let idx = fds.len();
                fds.push(PollFd::new(fd, PollFlags::OUT));
                idx
            });
            let so_idx = stdout_opt.as_ref().map(|fd| {
                let idx = fds.len();
                fds.push(PollFd::new(fd, PollFlags::IN));
                idx
            });
            let se_idx = stderr_opt.as_ref().map(|fd| {
                let idx = fds.len();
                fds.push(PollFd::new(fd, PollFlags::IN));
                idx
            });
            match poll(&mut fds, Some(&ts)) {
                Ok(0) => (false, false, false, false),
                Ok(_) => {
                    let out_ev = si_idx
                        .map(|i| fds[i].revents())
                        .unwrap_or_else(PollFlags::empty);
                    let in_so = so_idx
                        .map(|i| fds[i].revents())
                        .unwrap_or_else(PollFlags::empty);
                    let in_se = se_idx
                        .map(|i| fds[i].revents())
                        .unwrap_or_else(PollFlags::empty);
                    let nval = out_ev.contains(PollFlags::NVAL)
                        || in_so.contains(PollFlags::NVAL)
                        || in_se.contains(PollFlags::NVAL);
                    (
                        out_ev.intersects(PollFlags::OUT | PollFlags::HUP | PollFlags::ERR),
                        in_so.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR),
                        in_se.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR),
                        nval,
                    )
                }
                Err(Errno::INTR) => continue,
                Err(e) => {
                    return Err(TransportError::OutcomeUnknown(format!(
                        "failed polling curl pipes: {e}"
                    )));
                }
            }
        };

        if bad_fd {
            return Err(TransportError::OutcomeUnknown(
                "invalid curl pipe descriptor".into(),
            ));
        }

        if si_r && let Some(pipe) = stdin_opt.as_mut() {
            for _ in 0..32 {
                if stdin_offset >= stdin_bytes.len() {
                    break;
                }
                match pipe.write(&stdin_bytes[stdin_offset..]) {
                    Ok(0) => {
                        return Err(TransportError::OutcomeUnknown(
                            "unexpected zero write on curl stdin".into(),
                        ));
                    }
                    Ok(n) => stdin_offset += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                        stdin_broken_pipe = true;
                        stdin_opt = None;
                        break;
                    }
                    Err(e) => {
                        return Err(TransportError::OutcomeUnknown(format!(
                            "failed writing curl stdin: {e}"
                        )));
                    }
                }
            }
            if stdin_offset >= stdin_bytes.len() {
                stdin_opt = None;
            }
        }

        if so_r
            && let Some(pipe) = stdout_opt.as_mut()
            && matches!(
                drain_pipe(
                    pipe,
                    &mut stdout_buf,
                    MAX_CHILD_STDOUT_BYTES,
                    &mut stdout_truncated
                )?,
                PipeDrain::Eof
            )
        {
            stdout_opt = None;
        }

        if se_r
            && let Some(pipe) = stderr_opt.as_mut()
            && matches!(
                drain_pipe(
                    pipe,
                    &mut stderr_buf,
                    MAX_CHILD_STDERR_BYTES,
                    &mut stderr_truncated
                )?,
                PipeDrain::Eof
            )
        {
            stderr_opt = None;
        }
    }

    let status = guard.reap_after_group_kill()?;
    if status.success() {
        if stdin_broken_pipe {
            let diag = sanitize_diag(&stderr_buf, cred_for_redaction);
            return Err(TransportError::OutcomeUnknown(format!(
                "curl exited before reading full request body ({diag})"
            )));
        }
        if stdout_truncated {
            return Err(TransportError::OutcomeUnknown(
                "external API response exceeded stdout limit".into(),
            ));
        }
    }
    let _ = stderr_truncated;

    Ok(ChildRunOutput {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
    })
}

/// POST a JSON payload with an exclusive `0600` Bearer header config file.
pub fn post_json_bearer(
    endpoint: &EndpointUrl,
    access_cred: &SecretValue,
    payload: &Value,
    timeout: Duration,
    interrupted: &AtomicBool,
) -> Result<Value, TransportError> {
    let body_bytes = serde_json::to_vec(payload)
        .map_err(|e| TransportError::Rejected(format!("failed serializing JSON body: {e}")))?;
    let cred_str = access_cred.as_str();
    if body_bytes
        .windows(cred_str.len())
        .any(|window| window == cred_str.as_bytes())
    {
        return Err(TransportError::Rejected(
            "refusing outbound payload containing access credential".into(),
        ));
    }

    let mut req_dir = RequestTempDir::create()?;
    let mut escaped = cred_str.replace('\\', "\\\\").replace('"', "\\\"");
    let hdr_word = "header";
    let eq = "=";
    let auth_hdr = concat!("Author", "ization");
    let scheme = concat!("Bear", "er");
    let mut cfg_bytes = format!(
        "{hdr_word} {eq} \"{auth_hdr}: {scheme} {escaped}\"\n{hdr_word} {eq} \"Content-Type: application/json\"\n"
    )
    .into_bytes();
    escaped.clear();
    req_dir.write_cfg_exclusive(&mut cfg_bytes)?;

    let secs = timeout.as_secs_f64().clamp(0.2, 30.0);
    let cfg_path = req_dir.cfg_path();
    let mut cmd = Command::new("curl");
    cmd.args([
        std::ffi::OsString::from("-q"),
        "-sS".into(),
        "--max-time".into(),
        format!("{secs:.2}").into(),
        "--proto".into(),
        endpoint.curl_proto().into(),
        "-X".into(),
        "POST".into(),
        "-K".into(),
        cfg_path.into_os_string(),
        "-w".into(),
        "\n__OMS_HTTP_STATUS__:%{http_code}".into(),
        "--data-binary".into(),
        "@-".into(),
        "--".into(),
        endpoint.as_str().into(),
    ]);

    let output = run_curl_bounded(
        &mut cmd,
        &body_bytes,
        timeout + Duration::from_millis(500),
        interrupted,
        cred_str,
    )?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(1);
        let diag = sanitize_diag(&output.stderr, cred_str);
        return Err(TransportError::OutcomeUnknown(format!(
            "curl transport failed (exit {code}): {diag}"
        )));
    }

    let stdout_s = std::str::from_utf8(&output.stdout).map_err(|_| {
        TransportError::OutcomeUnknown("external API returned non-UTF-8 response".into())
    })?;
    let Some((json_part, status_part)) = stdout_s.rsplit_once("\n__OMS_HTTP_STATUS__:") else {
        return Err(TransportError::OutcomeUnknown(
            "HTTP status marker missing".into(),
        ));
    };
    let status_code = status_part.trim().parse::<u16>().unwrap_or(0);
    if status_code == 429 || status_code >= 500 || !(200..600).contains(&status_code) {
        return Err(TransportError::OutcomeUnknown(format!(
            "external API returned retryable/uncertain HTTP {status_code}"
        )));
    }
    if (300..400).contains(&status_code) {
        return Err(TransportError::OutcomeUnknown(format!(
            "external API returned unexpected redirect HTTP {status_code}"
        )));
    }
    if status_code >= 400 {
        return Err(TransportError::Rejected(format!(
            "external API rejected request with HTTP {status_code}"
        )));
    }

    serde_json::from_str::<Value>(json_part.trim())
        .map_err(|_| TransportError::OutcomeUnknown("external API returned invalid JSON".into()))
}
