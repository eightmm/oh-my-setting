//! Native tick schedule log and idle-main wake.
//!
//! A schedule only makes an already approved autopilot contract eligible for the
//! same resume path every wake takes; it never approves anything. Receipt and
//! approval authority is supplied through [`AutopilotHost`] so this crate keeps
//! no dependency on the autonomy command crate. Every file the tick owns
//! (`.oms/tick/schedules.jsonl`, `.oms/tick/last.json`) fails closed: an
//! unterminated, malformed, linked or oversized file is reported and left as
//! found, so a cap or a dedupe key never silently resets.
use crate::services::{ExecutionRequest, args, query};
use crate::{
    CommandOutput, Error, NativeCommand, Result, Services, Store, Transaction, Value, eventlog,
    release,
};
use regex::Regex;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Exit code, stdout, stderr: the shape the lifecycle tick service callbacks return.
pub type Output = (i32, Vec<u8>, Vec<u8>);

pub const SCHEDULE_FILE: &str = "schedules.jsonl";
pub const SCHEDULE_MAX: usize = 4 * 1024 * 1024;
pub const SCHEDULE_MAX_HOURS: i64 = 8760;
pub const WAKE_DAILY_LIMIT: usize = 6;
pub const WAKE_IDLE_SECONDS: i64 = 1800;
pub const TICK_RECEIPT_MAX: usize = 1024 * 1024;
const RESUME_SUBJECT: &str = "autopilot";
const CHILD_ROLES: [&str; 5] = ["worker", "advisor", "reviewer", "researcher", "delegate"];
const LAUNCH_HARNESSES: [&str; 4] = ["claude", "codex", "pi", "agy"];
const APPROVED_STAGES: [&str; 4] = ["approved", "driving", "reviewing", "publishing"];
/// A proposal held for a GitHub issue is not work a wake can move; only a restamp clears it.
const ISSUE_PENDING: &str = "autopilot proposal is pending an issue (GitHub project mode)";
const ISSUE_RESTAMP: &str = "the proposal is restamped with its issue (`oms autopilot propose --restamp PATH --expected-proposal-sha256 SHA --issue REF`)";
fn issue_held(receipt: &Value) -> bool {
    receipt["stage"] == "proposal-review" && receipt["blocked"] == "github-issue"
}

/// A receipt the host validated, with everything the resume decision reads.
#[derive(Clone, Debug)]
pub struct ApprovedContract {
    pub row: Value,
    /// SHA-256 of the receipt bytes; it moves on every stage transition.
    pub receipt_sha256: String,
    /// SHA-256 of the frozen fields only; ordinary progress never changes it.
    pub contract_digest: String,
    pub owner_id: String,
    pub stage: String,
    /// `oms autopilot ...` argv of the receipt's own continuation.
    pub argv: Vec<String>,
    /// The continuation is an approved `run --proposal FILE --expected-proposal-sha256 SHA`.
    pub approved_run: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContractError {
    /// No receipt file exists.
    Missing,
    Invalid(String),
}
/// Receipt and launch authority. `oms_command_autonomy::receipt` supplies the
/// read-only checks; this trait keeps the crates decoupled.
pub trait AutopilotHost {
    fn load_contract(&self, repo: &Path) -> std::result::Result<ApprovedContract, ContractError>;
    /// "" when the receipt may be re-entered, else why not (stage, digest gate, live claimant).
    fn reentry_refusal(
        &self,
        repo: &Path,
        contract: &ApprovedContract,
    ) -> std::result::Result<String, String>;
    /// "" when the approval may be resumed, else why not (`OMS_TICK_AUTOPILOT_MAX_AGE_DAYS`).
    fn stale_approval(&self, repo: &Path, contract: &ApprovedContract, now: i64) -> String;
    /// Current receipt-bound external wait. A refresh may query/reconcile its
    /// one producer, but must never submit work or call a model.
    fn external_wait(&self, _repo: &Path, _refresh: bool) -> std::result::Result<Value, String> {
        Err("external wait projection unavailable".into())
    }
    /// Native hosts pass this exact launch tuple to the child's existing
    /// progress producer. Legacy hosts remain usable, but cannot claim that a
    /// missing launch acknowledgement was delivered without that proof.
    fn launch_bound(
        &self,
        argv: &[String],
        log: &Path,
        cwd: &Path,
        _identity: &Value,
    ) -> std::result::Result<u32, String> {
        self.launch(argv, log, cwd)
    }
    fn tmux_available(&self) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("tmux").is_file()))
    }
    fn executable(&self) -> std::io::Result<PathBuf> {
        std::env::current_exe()
    }
    /// Start the approved continuation detached and return its pid without waiting.
    /// `argv` starts with `oms autopilot`; the executable replaces `oms`. The child
    /// gets its own process group, no stdin, the log as stdout and stderr, and
    /// `OMS_AUTOPILOT_WAKE_RESUME=1` so an unattended resume never refreshes the
    /// approval time. Rust std cannot create a new session without `unsafe`.
    fn launch(&self, argv: &[String], log: &Path, cwd: &Path) -> std::result::Result<u32, String> {
        use std::os::unix::process::CommandExt;
        let exe = self.executable().map_err(|e| e.to_string())?;
        let parent = log
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| format!("invalid launch log path {}", log.display()))?;
        let name = log
            .file_name()
            .ok_or_else(|| format!("invalid launch log name {}", log.display()))?;
        let dir = if parent == cwd.join(".oms/tick") {
            open_tick_dir(cwd, true)?.ok_or_else(|| "cannot open .oms/tick".to_owned())?
        } else {
            ensure_real_ancestors(log, "launch log")?;
            rustix::fs::open(
                parent,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|e| format!("cannot open launch log directory {}: {e}", parent.display()))?
        };
        let fd = rustix::fs::openat(
            &dir,
            name,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::APPEND
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
        let file = std::fs::File::from(fd);
        let meta = file
            .metadata()
            .map_err(|e| format!("cannot stat {}: {e}", log.display()))?;
        if !meta.is_file() || meta.nlink() != 1 || meta.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(format!(
                "refusing {}: launch log must be an owned single-link regular file",
                log.display()
            ));
        }
        let again = file.try_clone().map_err(|e| e.to_string())?;
        let mut child = std::process::Command::new(exe)
            .args(argv.iter().skip(1))
            .current_dir(cwd)
            .env("OMS_AUTOPILOT_WAKE_RESUME", "1")
            .stdin(std::process::Stdio::null())
            .stdout(file)
            .stderr(again)
            .process_group(0)
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }
}

// ---------------------------------------------------------------- shared text and time

fn cut(text: &str, limit: usize) -> String {
    eventlog::clean(text, usize::MAX)
        .chars()
        .take(limit)
        .collect()
}
fn utc(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
fn parse_utc(text: &str) -> Option<i64> {
    use chrono::{NaiveDate, NaiveDateTime};
    ["%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%dT%H:%MZ"]
        .iter()
        .find_map(|form| NaiveDateTime::parse_from_str(text, form).ok())
        .or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
        .map(|t| t.and_utc().timestamp())
}
fn compact(value: &Value) -> Result<String> {
    let mut text = String::from_utf8(crate::value::encoded(value)?).map_err(Error::new)?;
    text.pop();
    Ok(text)
}
fn hash16(parts: &[&str]) -> String {
    crate::digest(parts.join("\0").as_bytes())[..16].to_owned()
}

fn launch_progress(
    repo: &Path,
    id: &str,
    owner: &str,
    receipt: &str,
) -> std::result::Result<(bool, Option<String>), Fail> {
    let progress = Store::open(repo)
        .and_then(|s| s.read(Path::new(".oms/plan/progress.jsonl"), 4 * 1024 * 1024, true))
        .map_err(|e| Fail::Other(e.message))?;
    if !progress.is_empty() && !progress.ends_with(b"\n") {
        return Err(Fail::Other(
            "autopilot launch progress has a torn tail".into(),
        ));
    }
    let mut started = false;
    let mut finished = None;
    for raw in progress.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let event = crate::decode(raw, 1024 * 1024).map_err(|e| Fail::Other(e.message))?;
        if event["kind"] != "autopilot-wake-launch" || event["launch_id"] != id {
            continue;
        }
        if event["schema"] != 1
            || event["owner_id"] != owner
            || event["input_receipt_sha256"] != receipt
        {
            return Err(Fail::Other(
                "autopilot launch progress identity mismatch".into(),
            ));
        }
        match event["phase"].as_str() {
            Some("started") if !started && finished.is_none() => started = true,
            Some("finished")
                if started
                    && finished.is_none()
                    && event["exit"]
                        .as_i64()
                        .is_some_and(|n| (0..=255).contains(&n)) =>
            {
                finished = Some(crate::digest(raw))
            }
            _ => {
                return Err(Fail::Other(
                    "autopilot launch progress order is invalid".into(),
                ));
            }
        }
    }
    Ok((started, finished))
}
/// Python-style `shlex.quote`, so the printed continuation stays copyable.
fn quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// Failures keep Python's three kinds apart: a schedule log or tick receipt
/// that cannot be trusted refuses and is kept as found; anything else skips.
#[derive(Debug, Clone)]
enum Fail {
    Schedule(String),
    Ledger(String),
    Busy,
    Other(String),
}
enum SubmitError {
    PreInput(String),
    Uncertain(String),
}
impl Fail {
    fn text(&self) -> String {
        match self {
            Self::Schedule(m) | Self::Ledger(m) | Self::Other(m) => m.clone(),
            Self::Busy => "panel typing lock held".into(),
        }
    }
}
impl From<Error> for Fail {
    fn from(error: Error) -> Self {
        if error.code == 75 {
            Self::Busy
        } else {
            Self::Other(error.message)
        }
    }
}

fn ensure_real_ancestors(path: &Path, label: &str) -> std::result::Result<(), String> {
    for anc in [path.parent(), path.parent().and_then(Path::parent)]
        .into_iter()
        .flatten()
    {
        if anc.as_os_str().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(anc) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(format!("{label} ancestor cannot be inspected")),
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(format!(
                    "{label} ancestor is a symlink or not a regular directory"
                ));
            }
            Ok(_) => {}
        }
    }
    Ok(())
}

fn open_tick_dir(
    repo: &Path,
    create: bool,
) -> std::result::Result<Option<rustix::fd::OwnedFd>, String> {
    use rustix::fs::{Mode, OFlags, mkdirat, open, openat};
    use rustix::io::Errno;
    let root =
        std::fs::canonicalize(repo).map_err(|_| "repository root is unavailable".to_owned())?;
    let mut dir = open(
        &root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "repository root cannot be opened".to_owned())?;
    for seg in [".oms", "tick"] {
        if create {
            match mkdirat(&dir, seg, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(_) => return Err(format!("cannot create {seg} directory")),
            }
        }
        dir = match openat(
            &dir,
            seg,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if !create => return Ok(None),
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(format!("{seg} is a symlink or not a regular directory"));
            }
            Err(_) => return Err(format!("{seg} cannot be inspected or opened")),
        };
    }
    Ok(Some(dir))
}

fn read_regular_fd(
    mut file: std::fs::File,
    named: &rustix::fs::Stat,
    limit: usize,
    label: &str,
) -> std::result::Result<Vec<u8>, String> {
    let opened = rustix::fs::fstat(&file).map_err(|_| format!("{label} is unreadable"))?;
    let mut raw = Vec::new();
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| format!("{label} is unreadable"))?;
    if rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::RegularFile
        || (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino)
        || opened.st_nlink != 1
        || raw.len() > limit
    {
        return Err(format!("{label} changed while read or is too large"));
    }
    Ok(raw)
}

fn read_kept_in_tick(
    repo: &Path,
    name: &str,
    limit: usize,
    label: &str,
) -> std::result::Result<Option<Vec<u8>>, String> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
    use rustix::io::Errno;
    let Some(dir) = open_tick_dir(repo, false)? else {
        return Ok(None);
    };
    let named = match statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(format!("{label} cannot be inspected")),
        Ok(st) => st,
    };
    if FileType::from_raw_mode(named.st_mode) != FileType::RegularFile || named.st_nlink != 1 {
        return Err(format!(
            "{label} is a symlink, a hard link or not a regular file"
        ));
    }
    let fd = openat(
        &dir,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| format!("{label} is unreadable"))?;
    read_regular_fd(std::fs::File::from(fd), &named, limit, label).map(Some)
}

/// Bytes of a tick-owned file, or `None` only when it is missing; anything
/// untrusted (link, special file, changed while read, oversized) refuses.
fn read_kept(
    path: &Path,
    limit: usize,
    label: &str,
) -> std::result::Result<Option<Vec<u8>>, String> {
    use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, open, statat};
    use rustix::io::Errno;
    if let (Some(name), Some(tick_dir)) = (path.file_name().and_then(|n| n.to_str()), path.parent())
        && tick_dir.file_name() == Some(std::ffi::OsStr::new("tick"))
        && let Some(oms_dir) = tick_dir.parent()
        && oms_dir.file_name() == Some(std::ffi::OsStr::new(".oms"))
        && let Some(repo) = oms_dir.parent()
        && !repo.as_os_str().is_empty()
    {
        return read_kept_in_tick(repo, name, limit, label);
    }
    ensure_real_ancestors(path, label)?;
    let named = match statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(format!("{label} cannot be inspected")),
        Ok(st) => st,
    };
    if FileType::from_raw_mode(named.st_mode) != FileType::RegularFile || named.st_nlink != 1 {
        return Err(format!(
            "{label} is a symlink, a hard link or not a regular file"
        ));
    }
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| format!("{label} is unreadable"))?;
    read_regular_fd(std::fs::File::from(fd), &named, limit, label).map(Some)
}

// ---------------------------------------------------------------- wake ledger

fn receipt_path(repo: &Path) -> PathBuf {
    repo.join(".oms/tick/last.json")
}
/// (receipt, wakes) of the tick receipt; only a missing file is empty history.
fn read_tick_receipt(path: &Path) -> std::result::Result<(Value, Vec<Value>), String> {
    let Some(raw) = read_kept(path, TICK_RECEIPT_MAX, "tick receipt")? else {
        return Ok((json!({}), vec![]));
    };
    let receipt = crate::decode(&raw, TICK_RECEIPT_MAX)
        .map_err(|_| "tick receipt is empty or not JSON".to_owned())?;
    let wakes = match receipt.get("wakes") {
        None if receipt.is_object() => Some(vec![]),
        Some(Value::Array(rows)) => Some(rows.clone()),
        _ => None,
    };
    match wakes {
        Some(rows)
            if rows.iter().all(|r| {
                r["day"].as_str().is_some_and(|s| !s.is_empty())
                    && r["main"].as_str().is_some_and(|s| !s.is_empty())
                    && r["key"].as_str().is_some_and(|s| !s.is_empty())
                    && matches!(r["outcome"].as_str(), Some("woke" | "resumed" | "skipped"))
            }) =>
        {
            Ok((receipt, rows))
        }
        _ => Err("tick receipt or its wakes ledger is malformed".into()),
    }
}
fn lock_fail(error: oms_lock::Error) -> Fail {
    match error {
        oms_lock::Error::Busy => Fail::Busy,
        oms_lock::Error::Unsafe | oms_lock::Error::Changed => {
            Fail::Ledger("panel lock path is a symlink or changed while locking".into())
        }
        other => Fail::Other(other.to_string()),
    }
}
struct TickTx<'a> {
    tx: &'a Transaction<'a>,
    repo: &'a Path,
    receipt_guard: Option<&'a oms_lock::Guard>,
    expected_tick: std::cell::Cell<Option<(u64, u64)>>,
}
#[allow(clippy::unnecessary_cast)]
fn tick_dir_id(dir: &rustix::fd::OwnedFd) -> std::result::Result<(u64, u64), Fail> {
    let st = rustix::fs::fstat(dir)
        .map_err(|_| Fail::Ledger("tick directory cannot be inspected".into()))?;
    Ok((st.st_dev as u64, st.st_ino as u64))
}
fn tick_dir_state(repo: &Path) -> std::result::Result<Option<(u64, u64)>, Fail> {
    open_tick_dir(repo, false)
        .map_err(Fail::Ledger)?
        .as_ref()
        .map(tick_dir_id)
        .transpose()
}
impl TickTx<'_> {
    fn check(&self) -> std::result::Result<(), Fail> {
        self.tx.check_raw().map_err(lock_fail)?;
        if let Some(guard) = self.receipt_guard {
            guard.check().map_err(lock_fail)?;
        }
        if tick_dir_state(self.repo)? != self.expected_tick.get() {
            return Err(Fail::Ledger("tick directory changed while locked".into()));
        }
        Ok(())
    }
    fn ensure_tick_dir(&self) -> std::result::Result<(), Fail> {
        self.check()?;
        if self.expected_tick.get().is_none() {
            let dir = open_tick_dir(self.repo, true)
                .map_err(Fail::Schedule)?
                .ok_or_else(|| Fail::Other(".oms/tick is unavailable".into()))?;
            self.expected_tick.set(Some(tick_dir_id(&dir)?));
        }
        Ok(())
    }
}
fn check_tx(tx: Option<&TickTx<'_>>) -> std::result::Result<(), Fail> {
    let Some(tx) = tx else {
        return Ok(());
    };
    tx.check()
}
fn record_wake(
    tx: Option<&TickTx<'_>>,
    repo: &Path,
    receipt: &Value,
    wakes: Vec<Value>,
) -> std::result::Result<(), Fail> {
    let tx =
        tx.ok_or_else(|| Fail::Other("the wake ledger is written under the panel lock".into()))?;
    tx.ensure_tick_dir()?;
    let mut body = receipt.clone();
    body["wakes"] = Value::Array(wakes);
    let relative = receipt_path(repo);
    let relative = relative
        .strip_prefix(repo)
        .map_err(|e| Fail::Other(e.to_string()))?;
    tx.tx.write(
        relative,
        &crate::value::encoded(&body)?,
        false,
        TICK_RECEIPT_MAX,
    )?;
    Ok(())
}

/// One line for `oms tick status`: how much of today's wake allowance the tick spent.
pub fn wake_summary(wakes: &[Value], day: &str) -> String {
    let today: Vec<_> = wakes.iter().filter(|r| r["day"] == day).collect();
    let spent: Vec<_> = today.iter().filter(|r| r["outcome"] != "skipped").collect();
    let resumed = spent.iter().filter(|r| r["outcome"] == "resumed").count();
    let mut parts = vec![];
    if spent.len() > resumed {
        parts.push(format!("{} main prompt(s)", spent.len() - resumed));
    }
    if resumed > 0 {
        parts.push(format!("{resumed} autopilot resume(s)"));
    }
    let skipped = today.len() - spent.len();
    format!(
        "wakes today (UTC): {} of {WAKE_DAILY_LIMIT} spent{}{}; tokens, cost and wall time: not recorded",
        spent.len(),
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        },
        if skipped > 0 {
            format!("; {skipped} skipped")
        } else {
            String::new()
        },
    )
}
/// `oms tick status` helper: the summary line, or nothing when no wake was ever recorded.
pub fn wake_summary_cli(receipt: &Path, now: i64) -> Output {
    match read_tick_receipt(receipt) {
        Err(error) => (
            0,
            format!("wakes today (UTC): unknown ({error})\n").into_bytes(),
            vec![],
        ),
        Ok((_, wakes)) if wakes.is_empty() => (0, vec![], vec![]),
        Ok((_, wakes)) => (
            0,
            format!("{}\n", wake_summary(&wakes, &utc(now)[..10])).into_bytes(),
            vec![],
        ),
    }
}

// ---------------------------------------------------------------- schedule log

#[derive(Clone, Debug)]
struct State {
    contract: String,
    digest: String,
    every: i64,
    anchor: i64,
    paused: bool,
    until: Option<i64>,
    floor: i64,
    last_run: Option<(i64, i64)>,
    launch: Option<LaunchIntent>,
    suspended: bool,
    cancelled: bool,
}
#[derive(Clone, Debug)]
struct LaunchIntent {
    occurrence: i64,
    receipt: String,
    stage: String,
    id: String,
    /// Only an intent without a subsequent `launching` record proves that
    /// the host was never called. Neither elapsed time nor Err proves that.
    prepared: bool,
}
fn owner_id(text: &str) -> bool {
    text.strip_prefix("owner_")
        .is_some_and(|h| crate::hex(h, 32))
}
fn read_schedules(repo: &Path, limit: usize) -> std::result::Result<BTreeMap<String, State>, Fail> {
    let refuse = Fail::Schedule;
    let raw = read_kept_in_tick(repo, SCHEDULE_FILE, limit, "schedule log").map_err(refuse)?;
    let mut states = BTreeMap::new();
    let Some(raw) = raw else { return Ok(states) };
    if !raw.ends_with(b"\n") {
        return Err(Fail::Schedule(
            "schedule log is empty or its last event is unterminated".into(),
        ));
    }
    let malformed = || Fail::Schedule("schedule log is malformed".into());
    for line in raw[..raw.len() - 1].split(|b| *b == b'\n') {
        let event = crate::decode(line, limit).map_err(|_| malformed())?;
        let (Some(kind), Some(contract)) = (event["event"].as_str(), event["contract"].as_str())
        else {
            return Err(malformed());
        };
        if event["schema"].as_i64() != Some(1)
            || ![
                "create",
                "pause",
                "resume",
                "cancel",
                "fired",
                "skipped",
                "launch_intent",
                "launching",
                "launch_unknown",
                "launch_reconciled",
            ]
            .contains(&kind)
            || !owner_id(contract)
        {
            return Err(malformed());
        }
        let at = event["at"]
            .as_str()
            .and_then(parse_utc)
            .ok_or_else(malformed)?;
        if kind == "create" {
            let every = event["every_hours"]
                .as_i64()
                .filter(|n| (1..=SCHEDULE_MAX_HOURS).contains(n));
            let digest = event["digest"].as_str().filter(|d| crate::hex(d, 64));
            let anchor = event["anchor"].as_str().and_then(parse_utc);
            let until = match event.get("until") {
                None | Some(Value::Null) => Some(None),
                Some(v) => v.as_str().and_then(parse_utc).map(Some),
            };
            let (Some(every), Some(digest), Some(anchor), Some(until)) =
                (every, digest, anchor, until)
            else {
                return Err(malformed());
            };
            states.insert(
                contract.to_owned(),
                State {
                    contract: contract.to_owned(),
                    digest: digest.to_owned(),
                    every,
                    anchor,
                    paused: false,
                    until,
                    floor: anchor,
                    last_run: None,
                    launch: None,
                    suspended: false,
                    cancelled: false,
                },
            );
            continue;
        }
        let state = match states.get_mut(contract) {
            Some(s) if !s.cancelled => s,
            _ => return Err(malformed()),
        };
        match kind {
            "pause" | "resume" => state.paused = kind == "pause",
            "cancel" => state.cancelled = true,
            "launch_intent" | "launching" | "launch_unknown" => {
                let occurrence = event["occurrence"]
                    .as_str()
                    .and_then(parse_utc)
                    .ok_or_else(malformed)?;
                let receipt = event["receipt"]
                    .as_str()
                    .filter(|d| crate::hex(d, 64))
                    .ok_or_else(malformed)?;
                let stage = event["stage"]
                    .as_str()
                    .filter(|s| APPROVED_STAGES.contains(s))
                    .ok_or_else(malformed)?;
                let id = event["launch_id"]
                    .as_str()
                    .filter(|id| *id == hash16(&[RESUME_SUBJECT, contract, &utc(occurrence)]))
                    .ok_or_else(malformed)?;
                if kind == "launch_intent" {
                    if state.launch.is_some() || occurrence <= state.floor {
                        return Err(malformed());
                    }
                    state.floor = occurrence;
                    state.launch = Some(LaunchIntent {
                        occurrence,
                        receipt: receipt.into(),
                        stage: stage.into(),
                        id: id.into(),
                        prepared: true,
                    });
                } else {
                    let launch = state.launch.as_mut().ok_or_else(malformed)?;
                    if launch.occurrence != occurrence
                        || launch.receipt != receipt
                        || launch.stage != stage
                        || launch.id != id
                        || (kind == "launching" && !launch.prepared)
                        || (kind == "launch_unknown"
                            && (launch.prepared || !event["reason"].is_string()))
                    {
                        return Err(malformed());
                    }
                    launch.prepared = false;
                }
            }
            _ => {
                if kind == "skipped" && !event["reason"].is_string() {
                    return Err(malformed());
                }
                let occurrence = if matches!(kind, "fired" | "launch_reconciled")
                    || !event["occurrence"].is_null()
                {
                    Some(
                        event["occurrence"]
                            .as_str()
                            .and_then(parse_utc)
                            .ok_or_else(malformed)?,
                    )
                } else {
                    None
                };
                if let Some(o) = occurrence {
                    state.floor = state.floor.max(o);
                }
                if matches!(kind, "fired" | "launch_reconciled") {
                    if let Some(launch) = &state.launch {
                        if launch.prepared
                            || Some(launch.occurrence) != occurrence
                            || event["launch_id"] != launch.id
                            || event["receipt"] != launch.receipt
                            || event["stage"] != launch.stage
                            || (kind == "fired"
                                && !event["pid"]
                                    .as_u64()
                                    .is_some_and(|pid| pid > 1 && pid <= i32::MAX as u64))
                            || (kind == "launch_reconciled"
                                && (event["phase"] != "finished"
                                    || !event["source_sha256"]
                                        .as_str()
                                        .is_some_and(|v| crate::hex(v, 64))))
                        {
                            return Err(malformed());
                        }
                        state.launch = None;
                    } else if !event["launch_id"].is_null() {
                        return Err(malformed());
                    }
                    state.last_run = Some((at, occurrence.unwrap_or(at)));
                } else if event["reason"] == "digest-changed" {
                    state.suspended = true;
                }
            }
        }
    }
    Ok(states)
}
/// Whether the tick must consult the schedule path: a live (uncancelled,
/// unexpired) schedule, or a log that cannot be trusted and so must refuse.
pub fn schedules_pending(repo: &Path, now: Option<i64>) -> bool {
    let now = now.unwrap_or_else(|| crate::timestamp() as i64);
    match read_schedules(repo, SCHEDULE_MAX) {
        Err(_) => true,
        Ok(states) => states
            .values()
            .any(|s| !s.cancelled && s.until.is_none_or(|u| now < u)),
    }
}
/// One fsynced line; a kill mid-write leaves an unterminated line that every
/// later read refuses (fail closed).
fn append_schedule(
    repo: &Path,
    event: &Value,
    limit: usize,
    expected_tick: Option<(u64, u64)>,
) -> std::result::Result<(), Fail> {
    use rustix::fs::{Mode, OFlags, fsync, openat};
    use rustix::io::Errno;
    let tick_dir = open_tick_dir(repo, true)
        .map_err(Fail::Schedule)?
        .ok_or_else(|| Fail::Other(".oms/tick is unavailable".into()))?;
    if let Some(want) = expected_tick
        && tick_dir_id(&tick_dir)? != want
    {
        return Err(Fail::Ledger("tick directory changed while locked".into()));
    }
    let mut event = event.clone();
    event["schema"] = 1.into();
    let line = format!("{}\n", compact(&event)?);
    let fd = openat(
        &tick_dir,
        SCHEDULE_FILE,
        OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| match e {
        Errno::LOOP => Fail::Schedule("schedule log is a symlink".into()),
        _ => Fail::Other(format!("schedule log cannot be opened: {e}")),
    })?;
    let mut file = std::fs::File::from(fd);
    let opened = file.metadata().map_err(|e| Fail::Other(e.to_string()))?;
    if !opened.file_type().is_file()
        || opened.nlink() != 1
        || opened.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(Fail::Schedule(
            "schedule log is unowned, a hard link or not a regular file".into(),
        ));
    }
    if opened.len() as usize + line.len() > limit {
        return Err(Fail::Schedule(format!(
            "schedule log would exceed {limit} bytes; nothing was appended"
        )));
    }
    file.write_all(line.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| Fail::Other(format!("schedule log write failed: {e}")))?;
    let _ = fsync(&tick_dir);
    Ok(())
}
/// (due occurrence, next occurrence): missed occurrences coalesce into the latest due one.
fn schedule_occurrence(state: &State, now: i64) -> (Option<i64>, Option<i64>) {
    let step = state.every * 3600;
    let latest =
        (now >= state.anchor + step).then(|| state.anchor + (now - state.anchor) / step * step);
    if state.until.is_some_and(|u| now >= u) {
        return (None, None);
    }
    if let Some(l) = latest.filter(|l| *l > state.floor) {
        return (Some(l), Some(l));
    }
    let following = latest.unwrap_or(state.anchor) + step;
    (
        None,
        if state.until.is_some_and(|u| following >= u) {
            None
        } else {
            Some(following)
        },
    )
}
/// The state word `--list` and the resume share.
fn schedule_view(
    state: &State,
    now: i64,
    digest: Option<&str>,
    current: Option<&str>,
) -> &'static str {
    if state.suspended
        || (current == Some(state.contract.as_str()) && digest != Some(state.digest.as_str()))
    {
        "suspended"
    } else if state.until.is_some_and(|u| now >= u) {
        "expired"
    } else if state.paused {
        "paused"
    } else if current == Some(state.contract.as_str()) {
        "active"
    } else {
        "inactive"
    }
}

// ---------------------------------------------------------------- the runner

struct Env<'a> {
    repo: PathBuf,
    now: i64,
    host: &'a dyn AutopilotHost,
    services: &'a dyn Services,
    stop: &'a AtomicBool,
    schedule_max: usize,
    typing_gap: Duration,
    sweep_holds_receipt_lock: bool,
    #[cfg(test)]
    before_panel_lock: Option<&'a dyn Fn()>,
    #[cfg(test)]
    after_panel_lock: Option<&'a dyn Fn()>,
}
fn skip(why: impl AsRef<str>, until: impl AsRef<str>) -> String {
    format!("skipped: {}; resumes when {}", why.as_ref(), until.as_ref())
}
impl Env<'_> {
    fn day(&self) -> String {
        utc(self.now)[..10].to_owned()
    }
    fn schedules(&self) -> std::result::Result<BTreeMap<String, State>, Fail> {
        read_schedules(&self.repo, self.schedule_max)
    }
    fn schedules_checked(
        &self,
        tx: Option<&TickTx<'_>>,
    ) -> std::result::Result<BTreeMap<String, State>, Fail> {
        check_tx(tx)?;
        let states = self.schedules()?;
        check_tx(tx)?;
        Ok(states)
    }
    fn append(&self, tx: Option<&TickTx<'_>>, event: Value) -> std::result::Result<(), Fail> {
        if let Some(tx) = tx {
            tx.ensure_tick_dir()?;
        }
        append_schedule(
            &self.repo,
            &event,
            self.schedule_max,
            tx.and_then(|t| t.expected_tick.get()),
        )?;
        check_tx(tx)
    }
    /// (receipt, today's wake rows): the receipt keeps only today once rewritten.
    fn ledger(&self) -> std::result::Result<(Value, Vec<Value>), Fail> {
        let (receipt, wakes) =
            read_tick_receipt(&receipt_path(&self.repo)).map_err(Fail::Ledger)?;
        let day = self.day();
        Ok((
            receipt,
            wakes
                .into_iter()
                .filter(|r| r["day"] == day.as_str())
                .collect(),
        ))
    }
    fn ledger_checked(
        &self,
        tx: Option<&TickTx<'_>>,
    ) -> std::result::Result<(Value, Vec<Value>), Fail> {
        check_tx(tx)?;
        let res = self.ledger()?;
        check_tx(tx)?;
        Ok(res)
    }

    // ---- tmux, room and lifecycle evidence (read-only)

    fn tmux(&self, argv: &[&str]) -> std::result::Result<Option<String>, String> {
        let mut full = vec![OsString::from("tmux")];
        full.extend(args(argv));
        let mut request = ExecutionRequest::new(full, &self.repo);
        request.timeout = Duration::from_secs(3);
        request.output_limit = 2 * 1024 * 1024;
        let out: CommandOutput = self
            .services
            .execute(&request, self.stop)
            .map_err(|e| e.message)?;
        if !out.cleanup_complete || !out.output_complete || out.limited {
            return Err("tmux output is incomplete".into());
        }
        Ok((out.code == 0).then(|| String::from_utf8_lossy(&out.stdout).replace('\r', "")))
    }
    fn query(&self, command: NativeCommand, argv: &[&str]) -> std::result::Result<Value, String> {
        query(self.services, command, &self.repo, argv, self.stop).map_err(|e| e.message)
    }
    fn history(&self) -> std::result::Result<Vec<Value>, String> {
        match self.query(NativeCommand::Events, &["list", "--json"])? {
            Value::Array(rows) if rows.iter().all(Value::is_object) => Ok(rows),
            _ => Err("main lifecycle status unavailable".into()),
        }
    }
    fn repo_text(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }
    fn session_candidates(&self) -> Vec<String> {
        let name = self
            .repo
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let mut slug = String::new();
        let mut dash = true;
        for c in name.chars() {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                slug.push(c);
                dash = false;
            } else if !dash {
                slug.push('-');
                dash = true;
            }
        }
        let slug: String = slug.trim_matches('-').chars().take(32).collect();
        let slug = slug.trim_matches('-');
        let slug = if slug.is_empty() { "project" } else { slug };
        vec![
            format!("oms-{slug}"),
            format!(
                "oms-{slug}-{}",
                &crate::digest(self.repo_text().as_bytes())[..6]
            ),
        ]
    }
    /// The checkout recorded on an existing session, "" when unproven, `None` when absent.
    fn session_owner(&self, session: &str) -> Option<String> {
        self.tmux(&["has-session", "-t", &format!("={session}")])
            .ok()
            .flatten()?;
        Some(
            self.tmux(&[
                "show-options",
                "-v",
                "-t",
                &format!("={session}:"),
                "@oms_panel_repo",
            ])
            .ok()
            .flatten()
            .map(|s| s.trim().to_owned())
            .unwrap_or_default(),
        )
    }
    /// (session, owner): a name held by another checkout or a non-OMS session is never taken.
    fn resolve_session(&self) -> (String, Option<String>) {
        let names = self.session_candidates();
        let mut owner = None;
        for name in &names {
            owner = self.session_owner(name);
            if owner.is_none() || owner.as_deref() == Some(self.repo_text().as_str()) {
                return (name.clone(), owner);
            }
        }
        (names[names.len() - 1].clone(), owner)
    }
    fn recorded_room(&self, session: &str) -> std::result::Result<Option<String>, String> {
        let listed = self
            .tmux(&["show-environment", "-t", &format!("={session}")])?
            .ok_or("tmux session environment unavailable")?;
        let value = listed
            .lines()
            .find_map(|l| l.strip_prefix("OMS_ROOM_ID="))
            .unwrap_or_default();
        if value.is_empty() {
            Ok(None)
        } else {
            crate::identifier(value).map_err(|e| e.message)?;
            Ok(Some(value.to_owned()))
        }
    }
    /// Panel-wide joined mains with a live panel-main attempt, plus windows opened but
    /// not yet enrolled. `released` names attempts proven dead but not yet released.
    fn live_mains(
        &self,
        session: &str,
        room_id: Option<&str>,
        released: &BTreeSet<String>,
    ) -> std::result::Result<usize, String> {
        let unavailable =
            || "main window status unavailable; existing sessions preserved".to_owned();
        let listed = self
            .tmux(&[
                "list-windows",
                "-t",
                &format!("={session}"),
                "-F",
                "#{@oms_panel_owner}\t#{@oms_panel_room}\t#{@oms_panel_room_participant}\t#{window_name}\t#{window_id}\t#{@oms_panel_harness}",
            ])?
            .ok_or_else(unavailable)?;
        let records: Vec<Vec<&str>> = listed.lines().map(|l| l.split('\t').collect()).collect();
        let ids: BTreeSet<&str> = records.iter().filter_map(|r| r.get(4).copied()).collect();
        if records.is_empty()
            || records
                .iter()
                .any(|r| r.len() != 6 || r[3].trim().is_empty() || !target(r[4], '@'))
            || ids.len() != records.len()
        {
            return Err(unavailable());
        }
        let mut windows = vec![];
        for r in &records {
            let native = !r[2].is_empty()
                || !r[5].is_empty()
                || (!r[0].is_empty() && r[3] != "control")
                || LAUNCH_HARNESSES.contains(&r[3])
                || r[3] == "antigravity";
            if !native {
                if r[3] != "control" {
                    return Err(
                        "main window ownership is unverified; existing sessions preserved".into(),
                    );
                }
                continue;
            }
            windows.push((r[1].to_owned(), r[2].to_owned()));
        }
        let mut rooms: BTreeSet<String> = windows
            .iter()
            .filter(|w| !w.0.is_empty())
            .map(|w| w.0.clone())
            .collect();
        rooms.extend(room_id.filter(|r| !r.is_empty()).map(str::to_owned));
        let attempts = self.history().map_err(|_| {
            "main lifecycle status unavailable; existing sessions preserved".to_owned()
        })?;
        let (mut live, mut terminal) = (BTreeSet::new(), BTreeSet::new());
        for room in &rooms {
            let state = self
                .query(NativeCommand::Room, &["show", "--id", room, "--json"])
                .map_err(|_| {
                    "main lifecycle status unavailable; existing sessions preserved".to_owned()
                })?;
            let joined: BTreeSet<&str> = state["participants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["joined"] == true && p["role"] == "main")
                .filter_map(|p| p["participant"].as_str())
                .collect();
            for row in attempts.iter().filter(|r| {
                !released.contains(r["attempt_id"].as_str().unwrap_or_default())
                    && r["tool"] == "panel-main"
                    && r["refs"]["panel_room_id"] == room.as_str()
            }) {
                let who = row["refs"]["panel_room_participant"]
                    .as_str()
                    .or(row["attempt_id"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                if row["terminal"] == true {
                    terminal.insert((room.clone(), who));
                } else if joined.contains(who.as_str()) {
                    live.insert((room.clone(), who));
                }
            }
        }
        let mut unnamed = 0;
        for (room, member) in windows {
            if member.is_empty() {
                unnamed += 1;
            } else if !terminal.contains(&(room.clone(), member.clone())) {
                live.insert((room, member));
            }
        }
        Ok(live.len() + unnamed)
    }
    /// "" only when no panel main is live; otherwise why one may be. Unverified liveness counts
    /// as live: an open panel-main attempt blocks unless its host and native process are proven
    /// gone (release's own proof), and a session window for this checkout blocks while it holds a main.
    fn live_main_blocker(&self) -> String {
        match self.blocker() {
            Ok(text) => text,
            Err(error) => format!("main liveness unverified: {}", cut(&error, 160)),
        }
    }
    fn blocker(&self) -> std::result::Result<String, String> {
        let history = self.history()?;
        let dead: BTreeSet<String> =
            release::classify(&self.repo, &history, self.services)["releasable"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| i["attempt"].as_str().map(str::to_owned))
                .collect();
        if history.iter().any(|r| {
            r["tool"] == "panel-main"
                && r["terminal"] != true
                && !dead.contains(r["attempt_id"].as_str().unwrap_or_default())
        }) {
            return Ok(cut(&main_refusal(&history), 200));
        }
        if self.host.tmux_available() {
            let (session, owner) = self.resolve_session();
            if owner.as_deref() == Some(self.repo_text().as_str())
                && self.live_mains(&session, self.recorded_room(&session)?.as_deref(), &dead)? > 0
            {
                return Ok("a panel main window is live".into());
            }
        }
        Ok(String::new())
    }
    /// (window, attempt, pane, member, room) of the one live main window of the session.
    fn main_window(&self, session: &str) -> std::result::Result<[String; 5], String> {
        let listed = self
            .tmux(&[
                "list-windows",
                "-t",
                &format!("={session}"),
                "-F",
                "#{window_id}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}\t#{@oms_panel_room_participant}\t#{@oms_panel_room}\t#{@oms_panel_repo}",
            ])?
            .ok_or("main window status unavailable; swap refused")?;
        let attempt_id = Regex::new(r"^att_[0-9a-f]{32}$").map_err(|e| e.to_string())?;
        let mut found = vec![];
        for line in listed.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 6 || f[1].is_empty() {
                continue;
            }
            let (window, attempt, pane, member, room, owner) = (f[0], f[1], f[2], f[3], f[4], f[5]);
            if !attempt_id.is_match(attempt) {
                return Err("main window binding is unverified; swap refused".into());
            }
            let active = self
                .main_history(room)?
                .into_iter()
                .find(|r| r["attempt_id"] == attempt);
            if active.as_ref().is_some_and(|r| r["terminal"] == true) {
                continue;
            }
            if !target(window, '@')
                || !target(pane, '%')
                || owner != self.repo_text()
                || room.is_empty()
                || member.is_empty()
            {
                return Err("main window binding is unverified; swap refused".into());
            }
            if active.is_none_or(|r| r["terminal"] != false) {
                return Err("main lifecycle is unverified; swap refused".into());
            }
            found.push([window, attempt, pane, member, room].map(str::to_owned));
        }
        if found.len() == 1 {
            Ok(found.remove(0))
        } else {
            Err("swap requires exactly one live main window".into())
        }
    }
    /// Lifecycle rows of one room: its panel attempts and any row of an enrolled participant.
    fn main_history(&self, room: &str) -> std::result::Result<Vec<Value>, String> {
        let rows = self
            .history()
            .map_err(|_| "native owner evidence unavailable".to_owned())?;
        let state = self.query(NativeCommand::Room, &["show", "--id", room, "--json"])?;
        let enrolled: BTreeSet<&str> = state["participants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["participant"].as_str())
            .collect();
        Ok(rows
            .into_iter()
            .filter(|r| {
                r["refs"]["panel_room_id"] == room
                    || enrolled.contains(r["attempt_id"].as_str().unwrap_or_default())
                    || enrolled.contains(
                        r["refs"]["panel_room_participant"]
                            .as_str()
                            .unwrap_or_default(),
                    )
            })
            .collect())
    }
    /// (pane, harness) of the one verified working tmux panel main.
    fn live_main_pane(&self) -> std::result::Result<String, String> {
        let (session, owner) = self.resolve_session();
        if owner.as_deref() != Some(self.repo_text().as_str()) {
            return Err("no panel main to type into".into());
        }
        let [_, attempt, pane, member, room] = self.main_window(&session)?;
        if self.live_mains(&session, Some(&room), &BTreeSet::new())? != 1 {
            return Err("main liveness is unverified".into());
        }
        let history = self.main_history(&room)?;
        let old = history.iter().find(|r| r["attempt_id"] == attempt.as_str());
        let refs_ok = old.is_some_and(|o| {
            o["tool"] == "panel-main"
                && o["terminal"] == false
                && o["state"] == "working"
                && o["refs"]["panel_room_participant"]
                    .as_str()
                    .unwrap_or(&attempt)
                    == member
                && o["refs"]["panel_host"] == "tmux"
        });
        if !refs_ok {
            return Err("current main ownership is unverified".into());
        }
        let state = self.query(NativeCommand::Room, &["show", "--id", &room, "--json"])?;
        let provider = old.map_or(Value::Null, |o| o["provider"].clone());
        let bound = state["participants"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| {
                p["participant"] == member.as_str()
                    && p["role"] == "main"
                    && p["provider"] == provider
            });
        if !bound {
            return Err("current room main binding is unverified".into());
        }
        Ok(pane)
    }
    /// (pane, participant) of the verified main once it has no live child and has idled long enough.
    fn wake_target(&self) -> std::result::Result<(String, String, String), String> {
        let pane = self.live_main_pane()?;
        let (session, _) = self.resolve_session();
        let [_, attempt, main_pane, member, room] = self.main_window(&session)?;
        if main_pane != pane {
            return Err("main pane changed during verification".into());
        }
        if self.main_history(&room)?.iter().any(|r| {
            r["terminal"] == false
                && CHILD_ROLES.contains(&r["refs"]["panel_role"].as_str().unwrap_or_default())
        }) {
            return Err("main has a live child call".into());
        }
        crate::identifier(&member).map_err(|e| e.message)?;
        let store = Store::open(&self.repo).map_err(|e| e.message)?;
        let raw = store
            .read(
                Path::new(&format!(".oms/hooks/panel-activity/{member}.json")),
                4096,
                true,
            )
            .map_err(|e| e.message)?;
        let row = crate::decode(&raw, 4096).unwrap_or(Value::Null);
        let since = row["since"].as_f64();
        // No idle record (a provider without the activity hook) is not proof of idleness.
        if row["state"] != "idle"
            || row["attempt"] != attempt.as_str()
            || since.is_none_or(|s| {
                s < 0.0 || s > self.now as f64 || self.now as f64 - s < WAKE_IDLE_SECONDS as f64
            })
        {
            return Err(format!(
                "main is not idle for {} minutes",
                WAKE_IDLE_SECONDS / 60
            ));
        }
        Ok((pane, member, attempt))
    }
    /// Why an idle main should continue ("" when no shared work waits), and whether the
    /// autopilot proposal is held for a GitHub issue.
    fn wake_reason(&self, refresh: bool) -> std::result::Result<(String, bool), String> {
        let raw = Store::open(&self.repo)
            .and_then(|s| {
                s.read(
                    Path::new(".oms/plan/autopilot-run.json"),
                    TICK_RECEIPT_MAX,
                    true,
                )
            })
            .map_err(|e| e.message)?;
        let receipt = if raw.is_empty() {
            Value::Null
        } else {
            crate::decode(&raw, TICK_RECEIPT_MAX).map_err(|e| e.message)?
        };
        if receipt["stage"] == "parked" && receipt["diagnostic"]["reason_code"] == "parent-stopped"
        {
            return Ok((String::new(), false));
        }
        if receipt["stage"] == "parked" && !receipt["adaptive"]["wait"].is_null() {
            // A waiting contract suppresses generic ready-plan work. Polling
            // unfinished tasks must not turn an external wait into model calls.
            let wait = self.host.external_wait(&self.repo, refresh)?;
            if wait["owner_id"] != receipt["owner_id"]
                || wait["binding_sha256"] != receipt["adaptive"]["wait"]["binding_sha256"]
            {
                return Err("external wait custody changed".into());
            }
            let result = wait["result_sha256"].as_str().unwrap_or("");
            let reason = if wait["identity_valid"] == true
                && wait["status"] == "terminal"
                && wait["action"] == "resume"
                && crate::hex(result, 64)
                && receipt["adaptive"]["wait"]["consumed_result_sha256"] != result
            {
                format!(
                    "external result {result} for autopilot {} awaits owner decision",
                    receipt["owner_id"].as_str().unwrap_or("")
                )
            } else {
                String::new()
            };
            return Ok((reason, false));
        }
        let mut reasons = vec![];
        let listed = self
            .services
            .command(
                NativeCommand::Plan,
                &self.repo,
                &args(&["ready"]),
                self.stop,
            )
            .map_err(|e| e.message)?;
        let ready: BTreeSet<String> = if listed.code == 0 {
            String::from_utf8_lossy(&listed.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| cut(l, 80))
                .collect()
        } else {
            BTreeSet::new()
        };
        if !ready.is_empty() {
            let names: Vec<_> = ready.iter().take(8).cloned().collect();
            reasons.push(format!(
                "ready plan tasks {}{}",
                names.join(", "),
                if ready.len() > 8 {
                    format!(" (+{} more)", ready.len() - 8)
                } else {
                    String::new()
                }
            ));
        }
        let held = issue_held(&receipt);
        if receipt["stage"] == "proposal-review" && !held {
            reasons.push("an autopilot proposal waits in proposal-review".into());
        }
        if let Some(reason) = self.native_return_wake_reason()? {
            reasons.push(reason);
        }
        Ok((reasons.join("; "), held))
    }

    /// Observe bounded return metadata only. Import, live custody revalidation,
    /// evidence verification and every owner decision remain explicit operations.
    fn native_return_wake_reason(&self) -> std::result::Result<Option<String>, String> {
        let (session, _) = self.resolve_session();
        let [window, main, pane, member, room] = self.main_window(&session)?;
        let history = self.main_history(&room)?;
        let owner = history
            .iter()
            .find(|row| row["attempt_id"] == main && row["tool"] == "panel-main")
            .and_then(|row| row["provider"].as_str())
            .ok_or("native return Main owner is unavailable")?;
        let mut attempts: Vec<_> = history
            .iter()
            .filter(|row| {
                row["tool"] == "panel-native-assignment"
                    && row["provider"] == "codex"
                    && row["refs"]["panel_room_id"] == room
                    && ((row["terminal"] == false
                        && row["parent_attempt_id"] == main
                        && row["refs"]["panel_owner"] == owner
                        && row["refs"].get("native_succession_schema").is_none())
                        || (row["refs"]["native_custodian_attempt"] == main
                            && row["refs"]["native_resolution"] != "retired"))
                    && row["refs"]["native_assignment_schema"] == "1"
                    && row["refs"]["native_return_schema"] == "1"
            })
            .collect();
        if attempts.len() > 20 {
            return Err("native return observation exceeds 20 active assignments".into());
        }
        attempts.sort_by_key(|row| row["attempt_id"].as_str().unwrap_or_default());
        let store = Store::open(&self.repo).map_err(|e| e.message)?;
        let mut pending = vec![];
        for row in attempts {
            let refs = &row["refs"];
            match refs["native_transport"].as_str() {
                Some("intent") => continue,
                Some("submitted" | "sending" | "unknown") => {}
                _ => return Err("native return transport is unavailable".into()),
            }
            let metadata = crate::native_results::metadata(&store, row).map_err(|e| {
                format!(
                    "native return metadata unavailable: {}",
                    cut(&e.message, 120)
                )
            })?;
            let succession = crate::native_succession::metadata(&store, row).map_err(|e| {
                format!(
                    "native succession evidence unavailable: {}",
                    cut(&e.message, 120)
                )
            })?;
            if let Some(succession) = &succession {
                if succession["main_attempt_id"] != main
                    || succession["main_owner"] != owner
                    || succession["participant"] != member
                    || succession["room"] != room
                    || metadata["parent_participant"] != member
                {
                    return Err("native successor does not match the current Main and Room".into());
                }
                crate::native_succession::prove_successor(
                    &self.repo,
                    &history,
                    metadata["parent_attempt_id"].as_str().unwrap_or_default(),
                    succession["previous_main_attempt_id"]
                        .as_str()
                        .unwrap_or_default(),
                    &main,
                    &room,
                    &member,
                    self.services,
                    self.stop,
                )
                .map_err(|e| e.message)?;
                let stamp = self.pane_stamp(&pane)?;
                let pid = stamp[1]
                    .parse::<u32>()
                    .map_err(|_| "native successor pane PID unavailable")?;
                let process = crate::terminals::process(pid).map_err(|e| e.message)?;
                let session_id = self
                    .tmux(&["display-message", "-p", "-t", &pane, "#{session_id}"])?
                    .ok_or("native successor pane session unavailable")?;
                let session_proof = crate::terminals::provider_session_proof(
                    self.services,
                    &self.repo,
                    session_id.trim(),
                    self.stop,
                )
                .map_err(|e| e.message)?;
                if stamp[3] != main
                    || succession["main_pane"]["id"] != pane
                    || succession["main_pane"]["window"] != window
                    || succession["main_pane"]["pid"] != pid
                    || succession["main_pane"]["birth"] != process.birth
                    || succession["session"]
                        != serde_json::to_value(session_proof).map_err(|e| e.to_string())?
                {
                    return Err("native successor Main process or session custody changed".into());
                }
            }
            let result = metadata["artifact_sha256"].as_str().unwrap_or("");
            if succession
                .as_ref()
                .is_some_and(|s| s["mode"] == "retirement")
                || (succession.is_some() && refs["native_resolution"] == "retiring")
            {
                pending.push(json!({"attempt":row["attempt_id"],"task":row["task_id"],
                    "round":metadata["round"],"phase":"retirement","sha256":result,
                    "assignment_sha256":metadata["binding_sha256"],"main":main}));
                continue;
            }
            if row["terminal"] != false {
                continue;
            }
            let phase = match (
                metadata["status"].as_str(),
                refs["native_resolution"].as_str(),
                refs["native_resolution_result_sha256"]
                    .as_str()
                    .unwrap_or(""),
            ) {
                (Some("awaiting-report"), Some("awaiting-report"), "") => None,
                (Some("reported"), Some("pending"), "") => Some("reconciliation"),
                (Some("reported"), Some("request-revision"), digest) if digest == result => {
                    Some("continuation")
                }
                (Some("reported"), Some("blocked" | "reviewed"), digest) if digest == result => {
                    None
                }
                _ => return Err("native return resolution differs from its retained result".into()),
            };
            let input = match metadata["input_presence"]["manifest"].as_str() {
                Some("absent") => None,
                Some("present") => {
                    let path = metadata["input_paths"]["manifest"]
                        .as_str()
                        .ok_or("native return input path is unavailable")?;
                    let input =
                        crate::native_results::CaptureInput::read(&self.repo, Path::new(path))
                            .map_err(|e| {
                                format!("native return input unavailable: {}", cut(&e.message, 120))
                            })?;
                    if input.attempt() != row["attempt_id"].as_str().unwrap_or("")
                        || input.binding_digest()
                            != metadata["binding_sha256"].as_str().unwrap_or("")
                    {
                        return Err("native return input differs from its active assignment".into());
                    }
                    if metadata["input_sha256"] == input.input_digest() {
                        None
                    } else if refs["native_resolution"] == "reviewed" {
                        return Err(
                            "reviewed native return has changed input; owner inspection required"
                                .into(),
                        );
                    } else if input.previous_digest() == result {
                        if metadata["input_presence"]["source"] != "present" {
                            return Err("native return source is absent or unsafe; owner inspection required".into());
                        }
                        Some(input.input_digest().to_owned())
                    } else {
                        return Err(
                            "native return input predecessor differs from its retained result"
                                .into(),
                        );
                    }
                }
                _ => {
                    return Err(
                        "native return input presence is unknown; owner inspection required".into(),
                    );
                }
            };
            if let Some((phase, digest)) = input
                .as_deref()
                .map(|digest| ("import", digest))
                .or_else(|| phase.map(|phase| (phase, result)))
            {
                pending.push(json!({"attempt":row["attempt_id"],"task":row["task_id"],
                    "round":metadata["round"],"phase":phase,"sha256":digest,
                    "assignment_sha256":metadata["binding_sha256"],"main":main}));
            }
        }
        if pending.is_empty() {
            return Ok(None);
        }
        let names = pending
            .iter()
            .take(8)
            .map(|row| {
                format!(
                    "{} ({})",
                    cut(row["task"].as_str().unwrap_or(""), 80),
                    row["phase"].as_str().unwrap()
                )
            })
            .collect::<Vec<_>>();
        let digest = crate::digest(
            &crate::value::encoded(&Value::Array(pending.clone())).map_err(|e| e.message)?,
        );
        Ok(Some(format!(
            "native return awaits owner import/reconciliation/continuation: {}{}; observation {digest}; input remains untrusted until owner import; inspect oms panel --results --json",
            names.join(", "),
            if pending.len() > 8 {
                format!(" (+{} more)", pending.len() - 8)
            } else {
                String::new()
            },
        )))
    }

    // ---- the resume path

    /// No live panel main: run the receipt's own `autopilot run` continuation headlessly, once.
    /// Only a contract the user already approved resumes; the launch is recorded in the wake
    /// ledger (one pool of `WAKE_DAILY_LIMIT` per UTC day) before the spawn. A schedule's
    /// durable intent can be recovered before `launching`; once that record is durable,
    /// only a positive host acknowledgment records `fired`. Missing acknowledgments are
    /// uncertain and are never retried automatically.
    fn resume(&self, dry: bool, once: Option<&Path>, scheduled_only: bool) -> String {
        let outcome = if dry {
            self.resume_attempt(None, dry, once, scheduled_only)
        } else {
            // Main start takes this lock too, so no main can start between the liveness proof and the spawn.
            self.locked(Duration::from_millis(500), |tx| {
                self.resume_attempt(Some(tx), dry, once, scheduled_only)
            })
        };
        self.render(outcome)
    }
    /// Run under `.oms/tick/last.json` (when standalone, matching `lifecycle tick`) then
    /// `panel-main-start`; a busy panel lock is `Fail::Busy`.
    fn locked<T>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&TickTx<'_>) -> std::result::Result<T, Fail>,
    ) -> std::result::Result<T, Fail> {
        let store = Store::open(&self.repo)?;
        let pre_tick = tick_dir_state(store.repo())?;
        // Same lock order as lifecycle tick (`last.json` outside `panel-main-start`); sweeps pass
        // `sweep_holds_receipt_lock = true` because they already hold `last.json` across the callback.
        let receipt_guard = if self.sweep_holds_receipt_lock {
            None
        } else {
            let mut settings =
                oms_lock::Settings::from_env().map_err(|e| Fail::Other(e.to_string()))?;
            settings.timeout = timeout;
            let mut lock = oms_lock::Lock::for_file(&receipt_path(store.repo()), settings.clone())
                .map_err(|e| match e {
                    oms_lock::Error::Unsafe | oms_lock::Error::Changed => {
                        Fail::Ledger("tick lock path is a symlink or untrusted".into())
                    }
                    other => Fail::Other(other.to_string()),
                })?;
            if let Ok(rel) = lock.path().strip_prefix(store.repo()) {
                lock = oms_lock::Lock::in_root(store.repo().to_path_buf(), rel, settings).map_err(
                    |e| match e {
                        oms_lock::Error::Unsafe | oms_lock::Error::Changed => {
                            Fail::Ledger("tick lock path is a symlink or untrusted".into())
                        }
                        other => Fail::Other(other.to_string()),
                    },
                )?;
            }
            let guard = lock.acquire(None).map_err(|e| match e {
                oms_lock::Error::Busy => Fail::Other("tick maintenance lock held".into()),
                oms_lock::Error::Unsafe | oms_lock::Error::Changed => {
                    Fail::Ledger("tick lock path is a symlink or untrusted".into())
                }
                other => Fail::Other(other.to_string()),
            })?;
            Some(guard)
        };
        let guard_tick = tick_dir_state(store.repo())?;
        if let (Some(a), Some(b)) = (pre_tick, guard_tick)
            && a != b
        {
            return Err(Fail::Ledger("tick directory changed while locking".into()));
        }
        #[cfg(test)]
        if let Some(hook) = self.before_panel_lock {
            hook();
        }
        let tx = store
            .lock_at_raw(Path::new(".oms/artifacts/panel-main-start.lock"), timeout)
            .map_err(lock_fail)?;
        if let Some(ref guard) = receipt_guard {
            guard.check().map_err(lock_fail)?;
        }
        let after_tick = tick_dir_state(store.repo())?;
        if after_tick != guard_tick {
            return Err(Fail::Ledger("tick directory changed while locking".into()));
        }
        let tick_tx = TickTx {
            tx: &tx,
            repo: store.repo(),
            receipt_guard: receipt_guard.as_ref(),
            expected_tick: std::cell::Cell::new(after_tick),
        };
        #[cfg(test)]
        if let Some(hook) = self.after_panel_lock {
            hook();
        }
        f(&tick_tx)
    }
    fn render(&self, outcome: std::result::Result<String, Fail>) -> String {
        match outcome {
            Ok(line) => line,
            Err(Fail::Schedule(m)) => format!(
                "refused: {m}; .oms/tick/schedules.jsonl is kept as found: inspect it, then repair or move it aside (moving it drops its pauses and cancels)"
            ),
            Err(Fail::Ledger(m)) => format!(
                "refused: {m}; .oms/tick/last.json is kept as found: inspect it, then move it aside to reset today's wake ledger"
            ),
            Err(Fail::Busy) => "skipped: panel typing lock held".into(),
            Err(Fail::Other(m)) => format!("skipped: {}", cut(&m, 200)),
        }
    }
    fn resume_attempt(
        &self,
        tx: Option<&TickTx<'_>>,
        dry: bool,
        once: Option<&Path>,
        scheduled_only: bool,
    ) -> std::result::Result<String, Fail> {
        let (day, now, repo) = (self.day(), self.now, self.repo.display().to_string());
        let (ledger, wakes) = self.ledger_checked(tx)?;
        let schedules = self.schedules_checked(tx)?; // an untrusted log refuses whatever the receipt says
        if !dry {
            check_tx(tx)?;
        }
        let blocker = self.live_main_blocker();
        if !blocker.is_empty() {
            return Ok(skip(
                format!("a panel main may be live ({blocker})"),
                "it exits or its termination is proven",
            ));
        }
        if !dry {
            check_tx(tx)?;
        }
        let status = format!("`oms autopilot --repo {repo} status` shows a valid receipt");
        let invalid = |why: &str| {
            skip(
                format!("autopilot receipt invalid ({})", cut(why, 120)),
                format!("{status} or it is abandoned and re-proposed"),
            )
        };
        let contract = match self.host.load_contract(&self.repo) {
            Err(ContractError::Missing) => {
                return Ok(skip(
                    "no live main and no autopilot contract",
                    "`oms autopilot propose` and a reviewed `run --proposal` bind one",
                ));
            }
            Err(ContractError::Invalid(why)) => return Ok(invalid(&why)),
            Ok(c) => c,
        };
        let refusal = match self.host.reentry_refusal(&self.repo, &contract) {
            Ok(r) => r,
            Err(why) => return Ok(invalid(&why)),
        };
        if !dry {
            check_tx(tx)?;
        }
        let (stage, digest, owner) = (
            &contract.stage,
            &contract.receipt_sha256,
            &contract.owner_id,
        );
        if !ledger["pending_launch"].is_null() {
            let previous = &ledger["pending_launch"];
            let id = previous["launch_id"]
                .as_str()
                .filter(|s| crate::hex(s, 16))
                .ok_or_else(|| Fail::Ledger("pending launch id is malformed".into()))?;
            let bound = previous["receipt"]
                .as_str()
                .filter(|s| crate::hex(s, 64))
                .ok_or_else(|| Fail::Ledger("pending launch receipt is malformed".into()))?;
            if previous["owner_id"] != owner.as_str() {
                return Ok(
                    "skipped: an earlier owner's launch outcome is unknown; no resend".into(),
                );
            }
            let (started, finished) = launch_progress(&self.repo, id, owner, bound)?;
            if let Some(proof) = finished {
                if !dry {
                    let mut updated = ledger.clone();
                    updated["pending_launch"] = Value::Null;
                    updated["last_reconciled_launch"] =
                        json!({"launch_id":id,"source_sha256":proof});
                    record_wake(tx, &self.repo, &updated, wakes.clone())?;
                }
                return Ok(format!(
                    "{} autopilot launch {id} from exact start/finish records; no command was resent",
                    if dry { "would reconcile" } else { "reconciled" }
                ));
            }
            return Ok(format!(
                "skipped: autopilot launch {id} {}; no resend",
                if started {
                    "started without an exact finish record"
                } else {
                    "outcome unknown"
                }
            ));
        }
        let plan = (!owner.is_empty())
            .then(|| schedules.get(owner))
            .flatten()
            .filter(|p| !p.cancelled);
        if let Some(plan) = plan
            && schedule_view(plan, now, Some(&contract.contract_digest), Some(owner)) == "suspended"
        {
            // Recorded before any stage or claimant skip.
            if !dry && !plan.suspended {
                check_tx(tx)?;
                self.append(tx, json!({"event": "skipped", "contract": owner, "reason": "digest-changed", "digest": contract.contract_digest, "at": utc(now)}))?;
            }
            return Ok(skip(
                format!(
                    "schedule for {owner} suspended: the contract digest changed since it was scheduled"
                ),
                "the user reviews the contract, then `oms tick schedule --cancel` and schedules it again",
            ));
        }
        let pending = plan.and_then(|p| p.launch.as_ref());
        if let Some(launch) = pending {
            if !launch.prepared {
                let (started, finished) =
                    launch_progress(&self.repo, &launch.id, owner, &launch.receipt)?;
                if let Some(source) = finished {
                    if !dry {
                        self.append(tx, json!({"event":"launch_reconciled","contract":owner,"occurrence":utc(launch.occurrence),"receipt":launch.receipt,"stage":launch.stage,"launch_id":launch.id,"phase":"finished","source_sha256":source,"at":utc(now)}))?;
                    }
                    return Ok(format!(
                        "{} autopilot launch {} from its exact start/finish records; no command was resent",
                        if dry { "would reconcile" } else { "reconciled" },
                        launch.id
                    ));
                }
                if started {
                    return Ok(format!(
                        "skipped: autopilot launch {} started; no exact finish record, no resend",
                        launch.id
                    ));
                }
                return Ok(format!(
                    "skipped: schedule for {owner} launch {} outcome unknown; reconcile needed before another launch (occurrence {})",
                    launch.id,
                    utc(launch.occurrence)
                ));
            }
            if launch.receipt != *digest || launch.stage != *stage {
                return Ok(format!(
                    "skipped: schedule for {owner} prepared launch {} no longer matches the approved receipt; reconcile needed before retry",
                    launch.id
                ));
            }
        }
        if !refusal.is_empty() {
            let note = contract.row["diagnostic"]
                .as_object()
                .filter(|_| stage == "parked")
                .map(|d| {
                    format!(
                        "; next step: {} ({})",
                        cut(
                            d.get("reason").and_then(Value::as_str).unwrap_or("None"),
                            400
                        ),
                        cut(
                            d.get("reason_code")
                                .and_then(Value::as_str)
                                .unwrap_or("None"),
                            400
                        )
                    )
                })
                .unwrap_or_default();
            let after = match stage.as_str() {
                _ if issue_held(&contract.row) => ISSUE_RESTAMP.to_owned(),
                "proposal-review" => "the parent reviews the proposal and runs the printed `oms autopilot run --proposal FILE --expected-proposal-sha256 SHA`".to_owned(),
                "parked" => "the operator resolves the park (`oms autopilot --repo . status`) or abandons it".to_owned(),
                "done" => "a new `oms autopilot propose` binds the next contract".to_owned(),
                _ => format!("{status} and no live session holds the run"),
            };
            return Ok(skip(format!("autopilot {stage}: {refusal}{note}"), after));
        }
        if !contract.approved_run {
            return Ok(skip(
                format!("autopilot continuation at stage {stage} is not an approved `run`"),
                if issue_held(&contract.row) {
                    ISSUE_RESTAMP
                } else {
                    "the parent approves a proposal with `oms autopilot run --proposal FILE --expected-proposal-sha256 SHA`"
                },
            ));
        }
        let spent: Vec<&Value> = wakes.iter().filter(|w| w["outcome"] != "skipped").collect();
        if plan.is_none() && scheduled_only {
            return Ok(skip(
                format!(
                    "no schedule binds contract {}",
                    if owner.is_empty() {
                        "(no owner id)"
                    } else {
                        owner
                    }
                ),
                "`oms tick schedule --contract ID --every HOURS` binds one",
            ));
        }
        // The approval-age refusal, checked only once a resume would otherwise start; it is
        // recorded once per receipt state and day under its own key.
        let refuse_stale = || -> std::result::Result<Option<String>, Fail> {
            let stale = self.host.stale_approval(&self.repo, &contract, now);
            if stale.is_empty() {
                return Ok(None);
            }
            let key = hash16(&[RESUME_SUBJECT, "stale-approval", &day, digest]);
            if !dry
                && !wakes
                    .iter()
                    .any(|w| w["main"] == RESUME_SUBJECT && w["key"] == key.as_str())
            {
                let row = json!({"day": day, "main": RESUME_SUBJECT, "key": key, "outcome": "skipped", "reason": stale, "at": utc(now), "stage": stage, "receipt": digest});
                record_wake(tx, &self.repo, &ledger, [wakes.clone(), vec![row]].concat())?;
            }
            Ok(Some(skip(
                &stale,
                format!(
                    "a person re-approves it (`oms autopilot --repo {repo} reenter`), or OMS_TICK_AUTOPILOT_MAX_AGE_DAYS is raised (0 disables)"
                ),
            )))
        };
        let mut occurrence = None;
        let key;
        let mut budget: Option<(String, i64)> = None; // (owner, step) for an exhausted-budget consumption
        if let Some(plan) = plan {
            let view = schedule_view(plan, now, Some(&contract.contract_digest), Some(owner));
            let (due, following) = schedule_occurrence(plan, now);
            let due = pending.map(|l| l.occurrence).or(due);
            let named = format!("schedule for {owner}");
            if view == "expired" {
                return Ok(skip(
                    format!("{named} expired at {}", utc(plan.until.unwrap_or_default())),
                    "`oms tick schedule --cancel` or a new schedule after it",
                ));
            }
            if view == "paused" {
                return Ok(skip(
                    format!("{named} paused"),
                    format!("`oms tick schedule --resume --contract {owner}`"),
                ));
            }
            let Some(due) = due else {
                return Ok(skip(
                    format!("{named} not due"),
                    format!(
                        "the next occurrence is due at {}",
                        following.map_or("never (the schedule expires first)".to_owned(), utc)
                    ),
                ));
            };
            if let Some(refused) = refuse_stale()? {
                return Ok(refused);
            }
            occurrence = Some(due);
            key = hash16(&[RESUME_SUBJECT, owner, &utc(due)]);
            budget = Some((owner.clone(), plan.every * 3600));
            let reserved = spent
                .iter()
                .any(|w| w["main"] == RESUME_SUBJECT && w["key"] == key.as_str());
            if spent.len() >= WAKE_DAILY_LIMIT && !reserved {
                if pending.is_some() {
                    return Ok(skip(
                        "prepared autopilot launch waits for wake budget",
                        "the UTC day rolls over",
                    ));
                }
                if !dry {
                    check_tx(tx)?;
                }
                return self.budget(
                    tx,
                    dry,
                    owner,
                    due,
                    plan.every * 3600,
                    &format!("{} wakes already spent today", spent.len()),
                );
            }
        } else {
            if let Some(refused) = refuse_stale()? {
                return Ok(refused);
            }
            key = hash16(&[RESUME_SUBJECT, &day, digest]);
            if spent
                .iter()
                .any(|w| w["main"] == RESUME_SUBJECT && w["key"] == key.as_str())
            {
                return Ok(skip(
                    "autopilot already resumed today at this receipt state",
                    "the receipt changes or the UTC day rolls over",
                ));
            }
            if spent.len() >= WAKE_DAILY_LIMIT {
                return Ok(skip(
                    format!("{} wakes already spent today", spent.len()),
                    "the UTC day rolls over",
                ));
            }
        }
        let shown = contract
            .argv
            .iter()
            .map(|w| quote(w))
            .collect::<Vec<_>>()
            .join(" ");
        let scheduled = occurrence.map_or(String::new(), |o| {
            format!("schedule occurrence {}, ", utc(o))
        });
        let short = &digest[..12];
        if dry {
            return Ok(format!(
                "would resume autopilot ({scheduled}stage {stage}, receipt {short}): {shown}"
            ));
        }
        check_tx(tx)?;
        if let Some(marker) = once {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(marker)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if pending.is_some() {
                        return Ok(skip(
                            "this tick run already resumed one autopilot; prepared launch retained",
                            "the next tick run",
                        ));
                    }
                    return match (occurrence, &budget) {
                        (Some(o), Some((owner, step))) => self.budget(
                            tx,
                            dry,
                            owner,
                            o,
                            *step,
                            "this tick run already resumed one autopilot",
                        ),
                        _ => Ok(skip(
                            "this tick run already resumed one autopilot",
                            "the next tick run",
                        )),
                    };
                }
                Err(e) => {
                    return Ok(skip(
                        format!(
                            "cannot claim this tick run's resume marker ({})",
                            cut(&e.to_string(), 80)
                        ),
                        "the marker is writable",
                    ));
                }
            }
        }
        let log_relative = format!(".oms/tick/autopilot-{short}.log");
        let mut wake = json!({"day": day, "main": RESUME_SUBJECT, "key": key, "outcome": "resumed", "at": utc(now), "stage": stage, "receipt": digest});
        if let Some(o) = occurrence {
            if pending.is_none() {
                self.append(tx, json!({"event": "launch_intent", "contract": owner, "occurrence": utc(o), "receipt": digest, "stage": stage, "launch_id": key, "at": utc(now)}))?;
            }
            wake["schedule"] = utc(o).into();
            wake["launch_id"] = key.clone().into();
        }
        if !wakes
            .iter()
            .any(|w| w["main"] == RESUME_SUBJECT && w["key"] == key.as_str())
        {
            record_wake(
                tx,
                &self.repo,
                &ledger,
                [wakes.clone(), vec![wake]].concat(),
            )?;
        }
        if let Some(tx) = tx
            && tx.check().is_err()
        {
            return Ok(
                "skipped: autopilot launch aborted before host call (panel lock changed; prepared schedule intent can be retried)"
                    .into(),
            );
        }
        if let Some(o) = occurrence {
            // Publication is deliberately before the external effect. A kill
            // after this line cannot prove whether launch happened.
            self.append(tx, json!({"event": "launching", "contract": owner, "occurrence": utc(o), "receipt": digest, "stage": stage, "launch_id": key, "at": utc(now)}))?;
        } else {
            let (mut current, spent) = self.ledger_checked(tx)?;
            current["pending_launch"] =
                json!({"launch_id":key,"owner_id":owner,"receipt":digest,"stage":stage});
            record_wake(tx, &self.repo, &current, spent)?;
        }
        match self.host.launch_bound(
            &contract.argv,
            &self.repo.join(&log_relative),
            &self.repo,
            &json!({"launch_id":key,"owner_id":owner,"input_receipt_sha256":digest}),
        ) {
            Ok(pid) => {
                if let Some(o) = occurrence {
                    self.append(tx, json!({"event": "fired", "contract": owner, "occurrence": utc(o), "receipt": digest, "stage": stage, "launch_id": key, "pid": pid, "at": utc(now)}))?;
                } else {
                    let (mut current, spent) = self.ledger_checked(tx)?;
                    current["pending_launch"] = Value::Null;
                    record_wake(tx, &self.repo, &current, spent)?;
                }
                Ok(format!(
                    "resumed autopilot ({scheduled}stage {stage}, receipt {short}, pid {pid}, output {log_relative}): {shown}"
                ))
            }
            Err(why) => {
                if let Some(o) = occurrence {
                    self.append(tx, json!({"event": "launch_unknown", "contract": owner, "occurrence": utc(o), "receipt": digest, "stage": stage, "launch_id": key, "reason": cut(&why, 160), "at": utc(now)}))?;
                    Ok(format!(
                        "skipped: autopilot launch {key} outcome unknown; reconcile needed before retry ({})",
                        cut(&why, 160)
                    ))
                } else {
                    Ok(format!(
                        "skipped: autopilot launch failed after it was recorded ({})",
                        cut(&why, 160)
                    ))
                }
            }
        }
    }
    /// An exhausted budget consumes the occurrence: it is never queued.
    fn budget(
        &self,
        tx: Option<&TickTx<'_>>,
        dry: bool,
        owner: &str,
        occurrence: i64,
        step: i64,
        why: &str,
    ) -> std::result::Result<String, Fail> {
        if !dry {
            self.append(tx, json!({"event": "skipped", "contract": owner, "reason": "budget", "detail": why, "occurrence": utc(occurrence), "at": utc(self.now)}))?;
        }
        Ok(skip(
            format!(
                "schedule for {owner} occurrence {} skipped: {why}",
                utc(occurrence)
            ),
            format!(
                "the next occurrence at {} finds budget",
                utc(occurrence + step)
            ),
        ))
    }

    // ---- the typed wake

    /// At most one prompt into an idle panel main while shared work waits; with no live main it
    /// resumes an approved autopilot contract instead. The wake is recorded before it is typed,
    /// so a retried or killed tick never submits it twice. A pane whose input line is not provably
    /// empty is skipped and the skip recorded; nothing is cleared, typed or entered.
    fn wake_main(&self, dry: bool, once: Option<&Path>) -> String {
        let blocker = self.live_main_blocker();
        if blocker.is_empty() {
            return self.resume(dry, once, false);
        }
        if let Err(why) = self.live_main_pane() {
            // Neither a pane to type into nor proof the main is gone: say why.
            return format!(
                "skipped: no live main pane to type into ({}); {blocker}",
                cut(&why, 100)
            );
        }
        // Idle/custody checks precede even a bounded network observation.
        if let Err(why) = self.wake_target() {
            return format!("skipped: {}", cut(&why, 200));
        }
        let reason = match self.wake_reason(!dry) {
            Ok((r, held)) if r.is_empty() => {
                return if held {
                    skip(ISSUE_PENDING, ISSUE_RESTAMP)
                } else {
                    "skipped: no ready plan work, pending proposal or native return".into()
                };
            }
            Ok((r, _)) => r,
            Err(why) => return format!("skipped: {}", cut(&why, 200)),
        };
        let outcome = if dry {
            self.wake_attempt(None, dry, &reason)
        } else {
            // swap_main verifies and types under this lock; a wake must not interleave with a swap or start.
            self.locked(Duration::from_millis(500), |tx| {
                self.wake_attempt(Some(tx), dry, &reason)
            })
        };
        self.render(outcome)
    }
    fn wake_attempt(
        &self,
        tx: Option<&TickTx<'_>>,
        dry: bool,
        reason: &str,
    ) -> std::result::Result<String, Fail> {
        let (receipt, wakes) = self.ledger_checked(tx)?;
        let (pane, member, main_attempt) = self.wake_target().map_err(Fail::Other)?;
        if self.wake_reason(false).map_err(Fail::Other)?.0 != reason {
            return Ok("skipped: wake evidence changed before submission".into());
        }
        let external_key = reason
            .starts_with("external result ")
            .then(|| crate::digest(format!("{reason}\0{main_attempt}").as_bytes()));
        let external = match receipt.get("external_wakes") {
            None => vec![],
            Some(Value::Array(rows))
                if rows.len() <= 128
                    && rows
                        .iter()
                        .all(|v| v.as_str().is_some_and(|s| crate::hex(s, 64))) =>
            {
                rows.clone()
            }
            _ => return Err(Fail::Ledger("external wake history is malformed".into())),
        };
        if external_key
            .as_ref()
            .is_some_and(|key| external.iter().any(|v| v == key))
        {
            return Ok(format!(
                "skipped: {member} already woken for this exact external result"
            ));
        }
        if external_key.is_some() && external.len() == 128 {
            return Err(Fail::Ledger("external wake history bound reached".into()));
        }
        let day = self.day();
        let spent: Vec<&Value> = wakes.iter().filter(|w| w["outcome"] != "skipped").collect();
        let key = if external_key.is_some() {
            hash16(&[&member, &day, reason, &main_attempt])
        } else {
            hash16(&[&member, &day, reason])
        };
        if spent
            .iter()
            .any(|w| w["main"] == member.as_str() && w["key"] == key.as_str())
        {
            return Ok(format!(
                "skipped: {member} already woken today for this work"
            ));
        }
        if spent.len() >= WAKE_DAILY_LIMIT {
            return Ok(format!(
                "skipped: {member} reached {WAKE_DAILY_LIMIT} wakes today"
            ));
        }
        let prompt = wake_prompt(reason);
        let stamp = self.pane_stamp(&pane).map_err(Fail::Other)?;
        let (empty, why) = self.input_line_empty(&pane);
        if dry {
            return Ok(if empty {
                format!("would wake {member}: {prompt}")
            } else {
                format!("would skip {member}: {why}")
            });
        }
        check_tx(tx)?;
        let row = json!({"day": day, "main": member, "key": key, "at": utc(self.now)});
        let record = |outcome: &str, why: Option<&str>| -> std::result::Result<(), Fail> {
            let mut row = row.clone();
            row["outcome"] = outcome.into();
            if let Some(why) = why {
                row["reason"] = why.into();
            }
            let mut retained = receipt.clone();
            if let Some(key) = &external_key {
                let mut seen = external.clone();
                if outcome != "skipped" {
                    seen.push(key.clone().into());
                }
                retained["external_wakes"] = seen.into();
            }
            record_wake(
                tx,
                &self.repo,
                &retained,
                [wakes.clone(), vec![row]].concat(),
            )
        };
        if !empty {
            record("skipped", Some(&why))?;
            return Ok(format!("skipped: {member}; {why}"));
        }
        record("woke", None)?;
        if let Some(tx) = tx
            && tx.check().is_err()
        {
            return Ok(format!(
                "skipped: {member}; panel lock changed after wake was recorded; nothing was typed"
            ));
        }
        match self.submit_owned(tx, &pane, &stamp, &prompt) {
            Ok(()) => Ok(format!("woke {member}: {prompt}")),
            Err(SubmitError::PreInput(why)) => {
                // Nothing was typed before the pane changed hands, so "woke" must not stand
                // unless the held panel lock itself was already invalidated.
                match record("skipped", Some(&why)) {
                    Ok(()) => Ok(format!("skipped: {member}; {why}")),
                    Err(fail) => Ok(format!(
                        "skipped: {member}; {why} (recorded wake kept: {})",
                        cut(&fail.text(), 120)
                    )),
                }
            }
            Err(SubmitError::Uncertain(why)) => {
                // Prompt typing or Enter submission may have landed; keep the durable "woke" row.
                Ok(format!("skipped: {member}; {why}"))
            }
        }
    }

    // ---- tmux pane typing

    /// Who owns a native pane id right now: (pane id, pane pid, dead flag, main attempt, native pane).
    fn pane_stamp(&self, pane: &str) -> std::result::Result<Vec<String>, String> {
        let unowned = || "pane owner unverified; input refused".to_owned();
        let shown = self
            .tmux(&["display-message", "-p", "-t", pane, "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@oms_panel_main_attempt}\t#{@oms_panel_native_pane}"])
            .ok()
            .flatten()
            .ok_or_else(unowned)?;
        let f: Vec<String> = shown
            .trim_end_matches('\n')
            .split('\t')
            .map(str::to_owned)
            .collect();
        let attempt = Regex::new(r"^att_[0-9a-f]{32}$").map_err(|e| e.to_string())?;
        if f.len() != 5
            || f[0] != pane
            || f[4] != pane
            || f[2] != "0"
            || f[1].is_empty()
            || !f[1].bytes().all(|b| b.is_ascii_digit())
            || !attempt.is_match(&f[3])
        {
            return Err(unowned());
        }
        Ok(f)
    }
    /// Type into a native CLI and submit, but only while the pane still carries the stamp taken
    /// when it was judged typeable and its input line still proves empty right before typing.
    /// An Enter inside the typed burst reads as a pasted newline.
    fn submit_owned(
        &self,
        tx: Option<&TickTx<'_>>,
        pane: &str,
        stamp: &[String],
        text: &str,
    ) -> std::result::Result<(), SubmitError> {
        if self.pane_stamp(pane).ok().as_deref() != Some(stamp) {
            return Err(SubmitError::PreInput(
                "pane changed hands before input; nothing typed".into(),
            ));
        }
        let (empty, why) = self.input_line_empty(pane);
        if !empty {
            return Err(SubmitError::PreInput(why));
        }
        if let Some(tx) = tx
            && tx.check().is_err()
        {
            return Err(SubmitError::PreInput(
                "panel lock changed before input; nothing typed".into(),
            ));
        }
        let sent = |keys: &[&str]| -> std::result::Result<(), SubmitError> {
            let mut argv = vec!["send-keys", "-t", pane];
            argv.extend(keys);
            match self.tmux(&argv) {
                Ok(Some(_)) => Ok(()),
                _ => Err(SubmitError::Uncertain("send-keys failed".into())),
            }
        };
        sent(&["-l", text])?;
        std::thread::sleep(self.typing_gap);
        sent(&["Enter"])
    }
    /// (true, "") only when the live pane proves the native CLI's input line holds no user text.
    fn input_line_empty(&self, pane: &str) -> (bool, String) {
        let refuse = |why: &str| (false, why.to_owned());
        match self.tmux(&["display-message", "-p", "-t", pane, "#{pane_in_mode}"]) {
            Ok(Some(mode)) if mode.trim() == "0" => {}
            _ => return refuse("main pane is in copy mode or unreadable"),
        }
        match self.tmux(&["capture-pane", "-p", "-e", "-t", pane]) {
            Ok(Some(text)) if text.len() <= 1_048_576 => screen_input_empty(&text),
            _ => refuse("main pane capture unavailable"),
        }
    }
}

fn target(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}
fn main_refusal(history: &[Value]) -> String {
    let open: Vec<&Value> = history
        .iter()
        .filter(|r| {
            (r["tool"] == "panel-main" || r["panel"]["role"] == "main")
                && r["terminal"] != true
                && !["done", "failed", "cancelled", "timed_out", "abandoned"]
                    .contains(&r["state"].as_str().unwrap_or_default())
        })
        .collect();
    let occupying: Vec<&&Value> = open
        .iter()
        .filter(|r| {
            r["panel"]["role"] == "main"
                || r.get("refs").is_none()
                || (r["refs"]["panel_role"] == "main"
                    && release::heartbeat_fresh(r, crate::timestamp() as f64).unwrap_or(true))
        })
        .collect();
    let id = (occupying.len() == 1)
        .then(|| occupying[0]["attempt_id"].as_str())
        .flatten();
    match id.filter(|a| a.strip_prefix("att_").is_some_and(|h| crate::hex(h, 32))) {
        Some(attempt) => {
            let summary = "this panel already has a live main or unverified owner; if crashed:";
            let command = format!("oms panel --release-main {attempt}");
            if summary.len() + 1 + command.len() <= 140 {
                format!("{summary} {command}")
            } else {
                format!("{summary}\n{command}")
            }
        }
        None => "this panel already has a live main or unverified terminal owner".into(),
    }
}
/// One provider-neutral line; send-keys types it literally, so it must hold no newline.
fn wake_prompt(reason: &str) -> String {
    eventlog::clean(
        &format!(
            "OMS tick wake: {reason}. Continue the shared plan (oms agent-plan list) within your existing authority; if the next step needs the user's decision, authority you do not have, or a hard blocker, say which and stop."
        ),
        usize::MAX,
    )
}

// ---------------------------------------------------------------- input-line proof

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    placeholder: bool,
    inverse: bool,
}
const INPUT_SIDES: &str = "│┃|";
const INPUT_RULES: &str = "─━═╌┄╭╮╰╯┌┐└┘├┤┬┴┼";
fn sgr(state: &mut (bool, bool, bool), params: &str) {
    let codes: Vec<u32> = params
        .split([';', ':'])
        .map(|c| {
            if !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()) {
                c.parse().unwrap_or(u32::MAX)
            } else {
                0
            }
        })
        .collect();
    let (faint, inverse, grey) = state;
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        if code == 0 {
            (*faint, *inverse, *grey) = (false, false, false);
        } else if code == 2 || code == 22 {
            *faint = code == 2;
        } else if code == 7 || code == 27 {
            *inverse = code == 7;
        } else if (30..=37).contains(&code) || (90..=97).contains(&code) || code == 39 {
            *grey = code == 90;
        } else if (code == 38 || code == 48) && i + 2 < codes.len() && codes[i + 1] == 5 {
            if code == 38 {
                *grey = codes[i + 2] == 8 || (232..=250).contains(&codes[i + 2]);
            }
            i += 2;
        } else if (code == 38 || code == 48) && i + 4 < codes.len() && codes[i + 1] == 2 {
            if code == 38 {
                let rgb = &codes[i + 2..i + 5];
                let (hi, lo) = (
                    rgb.iter().max().copied().unwrap_or(0),
                    rgb.iter().min().copied().unwrap_or(0),
                );
                *grey = hi - lo <= 24 && (64..=200).contains(&hi);
            }
            i += 4;
        }
        i += 1;
    }
}
/// Per captured line, cells carrying (placeholder, inverse); attributes carry across lines as tmux emits them.
fn screen_cells(text: &str) -> Vec<Vec<Cell>> {
    let escape =
        Regex::new(r"\x1b\[([0-9;:?]*)([A-Za-z])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b.?")
            .expect("static pattern");
    let mut state = (false, false, false);
    let mut lines = vec![];
    for raw in text.split('\n') {
        let mut cells = vec![];
        let mut position = 0;
        let mut push = |slice: &str, state: &(bool, bool, bool)| {
            cells.extend(slice.chars().map(|ch| Cell {
                ch,
                placeholder: state.0 || state.2,
                inverse: state.1,
            }));
        };
        for m in escape.captures_iter(raw) {
            let whole = m.get(0).expect("whole match");
            push(&raw[position..whole.start()], &state);
            if m.get(2).is_some_and(|c| c.as_str() == "m") {
                sgr(&mut state, m.get(1).map_or("", |p| p.as_str()));
            }
            position = whole.end();
        }
        push(&raw[position..], &state);
        lines.push(cells);
    }
    lines
}
/// Out of copy mode, the input region is the last visible prompt-marker line (`>`, `❯` or `›`,
/// optionally inside a box) and the lines around it up to the nearest border or rule line. The
/// region must hold exactly one marker line; after the marker, that line and every later region
/// line may hold only blanks, box sides and dim or grey placeholder text (cursorless SGR dim/grey
/// matches `scripts/lib/panel_native.py` and `tests/tick-smoke.sh` empty-placeholder parity). Any
/// drawn cursor (`inverse` cell, including `\x1b[7m \x1b[0m`) counts as empty only as a lone block
/// cursor at column 0 of an otherwise blank input line, or at column 0 directly over the first
/// character before dim or grey placeholder text; anywhere else it is a typed character or draft.
/// Ambiguous screens refuse.
pub fn screen_input_empty(text: &str) -> (bool, String) {
    let mark_re = Regex::new(r"^(?:\s*[│┃|])?\s*[>❯›](?:[ \x{A0}]|$)").expect("static pattern");
    let lines = screen_cells(&text.replace('\r', ""));
    let plain: Vec<String> = lines
        .iter()
        .map(|c| c.iter().map(|x| x.ch).collect())
        .collect();
    let marks: Vec<usize> = (0..plain.len())
        .filter(|i| mark_re.is_match(&plain[*i]))
        .collect();
    let Some(&mark) = marks.last() else {
        return (
            false,
            "no input prompt line visible in the main pane".into(),
        );
    };
    let rules: Vec<usize> = (0..plain.len())
        .filter(|i| {
            plain[*i].trim().chars().any(|c| INPUT_RULES.contains(c))
                && plain[*i]
                    .chars()
                    .all(|c| INPUT_RULES.contains(c) || INPUT_SIDES.contains(c) || c == ' ')
        })
        .collect();
    let top = rules
        .iter()
        .filter(|r| **r < mark)
        .max()
        .map_or(-1, |r| *r as i64);
    let bottom = rules
        .iter()
        .filter(|r| **r > mark)
        .min()
        .copied()
        .unwrap_or(lines.len());
    if marks.iter().any(|r| top < *r as i64 && *r < mark) {
        return (
            false,
            "more than one prompt line in the main input region".into(),
        );
    }
    let after = mark_re
        .find(&plain[mark])
        .map_or(0, |m| plain[mark][..m.end()].chars().count());
    let mut rest: Vec<Cell> = lines[mark][after..].to_vec();
    while rest.last().is_some_and(|c| !c.inverse && c.ch == ' ') {
        rest.pop();
    }
    if rest
        .last()
        .is_some_and(|c| !c.inverse && INPUT_SIDES.contains(c.ch))
    {
        rest.pop();
        while rest.last().is_some_and(|c| !c.inverse && c.ch == ' ') {
            rest.pop();
        }
    }
    let holds = "main input line holds text".to_owned();
    if rest.len() == 1 && rest[0].ch == ' ' && rest[0].inverse {
        rest.clear();
    } else if let Some(first) = rest.iter().position(|c| c.ch != ' ' || c.inverse)
        && rest[first].inverse
    {
        let next = rest.get(first + 1).copied().unwrap_or(Cell {
            ch: ' ',
            placeholder: false,
            inverse: false,
        });
        if first != 0
            || rest[first].ch == ' '
            || next.ch == ' '
            || !next.placeholder
            || next.inverse
        {
            return (false, holds);
        }
        rest.drain(..=first);
    }
    if rest
        .iter()
        .any(|c| c.inverse || (c.ch != ' ' && !c.placeholder))
    {
        return (false, holds);
    }
    for cells in &lines[mark + 1..bottom.max(mark + 1)] {
        if cells
            .iter()
            .any(|c| c.inverse || (c.ch != ' ' && !INPUT_SIDES.contains(c.ch) && !c.placeholder))
        {
            return (
                false,
                "main input may continue below its prompt line".into(),
            );
        }
    }
    (true, String::new())
}

// ---------------------------------------------------------------- public entries

fn physical(repo: &Path) -> PathBuf {
    std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_owned())
}
fn env<'a>(
    repo: &Path,
    now: i64,
    host: &'a dyn AutopilotHost,
    services: &'a dyn Services,
    stop: &'a AtomicBool,
) -> Env<'a> {
    Env {
        repo: physical(repo),
        now,
        host,
        services,
        stop,
        schedule_max: SCHEDULE_MAX,
        typing_gap: Duration::from_millis(800),
        sweep_holds_receipt_lock: false,
        #[cfg(test)]
        before_panel_lock: None,
        #[cfg(test)]
        after_panel_lock: None,
    }
}
fn interrupted(stop: &AtomicBool) -> Option<Output> {
    stop.load(Ordering::Acquire)
        .then(|| (130, vec![], b"error: tick operation interrupted\n".to_vec()))
}
fn line(text: String) -> Output {
    (0, format!("{text}\n").into_bytes(), vec![])
}

/// `oms tick wake REPO [--dry-run] [--scheduled] [--once MARKER]`: one status line.
/// `--scheduled` is the tick without `OMS_TICK_WAKE`: only a due schedule resumes.
pub fn wake_cli(
    argv: &[OsString],
    host: &dyn AutopilotHost,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Output {
    let usage = || {
        (
            2,
            vec![],
            b"usage: wake REPO [--dry-run] [--scheduled] [--once MARKER]\n".to_vec(),
        )
    };
    let Some((repo, mut rest)) = argv.split_first() else {
        return usage();
    };
    let (mut dry, mut scheduled, mut once) = (false, false, None);
    while let Some((flag, tail)) = rest.split_first() {
        match flag.to_str() {
            Some("--dry-run") => (dry, rest) = (true, tail),
            Some("--scheduled") => (scheduled, rest) = (true, tail),
            Some("--once") => match tail.split_first() {
                Some((marker, tail)) => (once, rest) = (Some(PathBuf::from(marker)), tail),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    if let Some(out) = interrupted(stop) {
        return out;
    }
    let env = env(
        Path::new(repo),
        crate::timestamp() as i64,
        host,
        services,
        stop,
    );
    line(if scheduled {
        env.resume(dry, once.as_deref(), true)
    } else {
        env.wake_main(dry, once.as_deref())
    })
}

/// The sweep's wake callback: stdout is the canonical wake receipt
/// `{"wake":0|1,"status":LINE,"wakes":[...]}` where `wakes` is the daily ledger exactly as
/// `.oms/tick/last.json` now holds it, so the caller can prove no current-day entry vanished.
/// `wakes` is omitted only when that ledger is untrusted (the status then says `refused:`).
pub fn wake_idle_main_json(
    scheduled_only: bool,
    repo: &Path,
    host: &dyn AutopilotHost,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Output {
    wake_idle_main_json_once(scheduled_only, repo, host, services, stop, None)
}
/// [`wake_idle_main_json`] with the sweep's per-run marker: the first resume of the run creates
/// `once`, so one tick run starts at most one headless autopilot across every repo it sweeps.
pub fn wake_idle_main_json_once(
    scheduled_only: bool,
    repo: &Path,
    host: &dyn AutopilotHost,
    services: &dyn Services,
    stop: &AtomicBool,
    once: Option<&Path>,
) -> Output {
    wake_json_at(
        scheduled_only,
        repo,
        host,
        services,
        stop,
        crate::timestamp() as i64,
        once,
    )
}
fn wake_json_at(
    scheduled_only: bool,
    repo: &Path,
    host: &dyn AutopilotHost,
    services: &dyn Services,
    stop: &AtomicBool,
    now: i64,
    once: Option<&Path>,
) -> Output {
    if let Some(out) = interrupted(stop) {
        return out;
    }
    let mut env = env(repo, now, host, services, stop);
    // Called inside `lifecycle tick`, which already holds `.oms/tick/last.json`'s lock across the sweep.
    env.sweep_holds_receipt_lock = true;
    let mut status = if scheduled_only {
        env.resume(false, once, true)
    } else {
        env.wake_main(false, once)
    };
    let ledger = read_tick_receipt(&receipt_path(&env.repo));
    if let Err(why) = &ledger
        && !status.starts_with("refused:")
    {
        status = env.render(Err(Fail::Ledger(why.clone())));
    }
    let woke = ledger.is_ok()
        && (status.starts_with("resumed ") || (!scheduled_only && status.starts_with("woke ")));
    let mut receipt = json!({"wake": i32::from(woke), "status": status});
    if let Ok((_, wakes)) = ledger {
        receipt["wakes"] = Value::Array(wakes);
    }
    match crate::value::encoded(&receipt) {
        Ok(body) => (0, body, vec![]),
        Err(e) => (2, vec![], format!("error: {e}\n").into_bytes()),
    }
}

// ---------------------------------------------------------------- `oms tick schedule`

const SCHEDULE_USAGE: &str = "usage: oms tick schedule [-h] [--repo REPO] [--contract CONTRACT] [--every HOURS] [--until DATE] [--pause | --resume | --cancel | --list]";
struct Request {
    repo: String,
    contract: Option<String>,
    every: Option<String>,
    until: Option<String>,
    action: Option<&'static str>,
}
fn schedule_parse(args: &[OsString]) -> std::result::Result<Request, String> {
    let mut out = Request {
        repo: ".".into(),
        contract: None,
        every: None,
        until: None,
        action: None,
    };
    let mut it = args.iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(word) = it.next() {
        let (name, inline) = match word.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
            _ => (word.clone(), None),
        };
        let value = |it: &mut dyn Iterator<Item = String>| {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or(format!("argument {name}: expected one argument"))
        };
        match name.as_str() {
            "--repo" => out.repo = value(&mut it)?,
            "--contract" => out.contract = Some(value(&mut it)?),
            "--every" => out.every = Some(value(&mut it)?),
            "--until" => out.until = Some(value(&mut it)?),
            "--pause" | "--resume" | "--cancel" | "--list" => {
                let action = ["pause", "resume", "cancel", "list"]
                    .into_iter()
                    .find(|a| name[2..] == **a)
                    .expect("listed");
                if let Some(old) = out.action.filter(|old| *old != action) {
                    return Err(format!(
                        "argument {name}: not allowed with argument --{old}"
                    ));
                }
                out.action = Some(action);
            }
            _ => return Err(format!("unrecognized arguments: {word}")),
        }
    }
    Ok(out)
}
/// `oms tick schedule`: create, pause, resume, cancel or list cadence events for an approved
/// autopilot contract. `args` is the tick argument vector (a leading `schedule` is dropped);
/// `repo_default` resolves a relative `--repo`. Mutating actions refuse inside a harness child.
pub fn schedule_cli(
    args: &[OsString],
    repo_default: &Path,
    host: &dyn AutopilotHost,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Output {
    let knobs = Knobs {
        now: crate::timestamp() as i64,
        child: std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|v| v == "1"),
        limit: SCHEDULE_MAX,
        wait: Duration::from_secs(5),
    };
    schedule_at(args, repo_default, host, services, stop, &knobs)
}
/// Everything `schedule_cli` reads from the process, so tests can set it without touching the environment.
#[derive(Clone, Copy)]
struct Knobs {
    now: i64,
    child: bool,
    limit: usize,
    wait: Duration,
}
fn schedule_at(
    args: &[OsString],
    repo_default: &Path,
    host: &dyn AutopilotHost,
    services: &dyn Services,
    stop: &AtomicBool,
    knobs: &Knobs,
) -> Output {
    let Knobs {
        now,
        child,
        limit,
        wait,
    } = *knobs;
    let usage = |message: &str| {
        (
            2,
            vec![],
            format!("{SCHEDULE_USAGE}\noms tick schedule: error: {message}\n").into_bytes(),
        )
    };
    let args = if args.first().is_some_and(|a| a == "schedule") {
        &args[1..]
    } else {
        args
    };
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return (0, format!("{SCHEDULE_USAGE}\n\nResume an approved autopilot contract every HOURS (whole hours >= 1) through the tick's resume path. Missed occurrences coalesce into one run; a paused schedule resumes with that one coalesced run.\n").into_bytes(), vec![]);
    }
    let request = match schedule_parse(args) {
        Ok(r) => r,
        Err(m) => return usage(&m),
    };
    let act = request
        .action
        .or(request.every.is_some().then_some("create"));
    let Some(act) = act else {
        return usage("give --every HOURS, --pause, --resume, --cancel or --list");
    };
    if act == "list"
        && (request.contract.is_some() || request.every.is_some() || request.until.is_some())
    {
        return usage("--list takes only --repo");
    }
    if matches!(act, "pause" | "resume" | "cancel")
        && (request.every.is_some() || request.until.is_some())
    {
        return usage(&format!("--{act} takes only --contract and --repo"));
    }
    let contract = request.contract.clone().unwrap_or_default();
    if act != "list" && !owner_id(&contract) {
        return usage("--contract must be the receipt's owner id (owner_ plus 32 hex digits)");
    }
    let (mut every, mut until) = (0, None);
    if act == "create" {
        let raw = request.every.as_deref().unwrap_or_default();
        match raw.parse::<i64>().ok().filter(|n| {
            (1..=SCHEDULE_MAX_HOURS).contains(n)
                && raw.len() <= 5
                && raw.bytes().all(|b| b.is_ascii_digit())
        }) {
            Some(n) => every = n,
            None => {
                return usage(&format!(
                    "--every must be whole hours from 1 to {SCHEDULE_MAX_HOURS}"
                ));
            }
        }
        if let Some(text) = &request.until {
            match parse_utc(text) {
                Some(u) => until = Some(u),
                None => {
                    return usage(
                        "--until must be UTC YYYY-MM-DD, YYYY-MM-DDTHH:MMZ or YYYY-MM-DDTHH:MM:SSZ",
                    );
                }
            }
        }
    }
    let given = Path::new(&request.repo);
    let given = if given.is_absolute() {
        given.to_owned()
    } else {
        repo_default.join(given)
    };
    let top = crate::git::run(
        services,
        &given,
        &["rev-parse", "--show-toplevel"],
        None,
        None,
        stop,
    )
    .ok()
    .filter(|o| o.code == 0)
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let repo = physical(&top.map_or(given, PathBuf::from));
    match std::fs::symlink_metadata(repo.join(".oms")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                2,
                vec![],
                format!("error: not an adopted repo (no .oms): {}\n", repo.display()).into_bytes(),
            );
        }
        Ok(m) if m.file_type().is_dir() => {}
        _ => {
            return (
                1,
                vec![],
                b"error: .oms is a symlink or not a regular directory\n".to_vec(),
            );
        }
    }
    if act != "list" && child {
        return (2, vec![], b"error: a harness child cannot schedule parent-owned autopilot runs; return the request to the parent agent\n".to_vec());
    }
    if let Some(out) = interrupted(stop) {
        return out;
    }
    let mut env = env(&repo, now, host, services, stop);
    env.schedule_max = limit;
    let result = if act == "list" {
        env.schedule_list()
    } else {
        env.locked(wait, |tx| {
            env.schedule_change(Some(tx), act, &contract, every, until)
        })
    };
    match result {
        Ok(text) => (0, text.into_bytes(), vec![]),
        Err(fail) => (
            1,
            vec![],
            format!("error: {}\n", cut(&fail.text(), 240)).into_bytes(),
        ),
    }
}
impl Env<'_> {
    fn current_contract(&self) -> std::result::Result<ApprovedContract, Fail> {
        self.host.load_contract(&self.repo).map_err(|e| {
            Fail::Other(format!(
                "autopilot receipt unavailable ({})",
                match e {
                    ContractError::Missing => "no receipt".to_owned(),
                    ContractError::Invalid(m) => cut(&m, 120),
                }
            ))
        })
    }
    fn schedule_change(
        &self,
        tx: Option<&TickTx<'_>>,
        act: &str,
        contract: &str,
        every: i64,
        until: Option<i64>,
    ) -> std::result::Result<String, Fail> {
        let now = self.now;
        let states = self.schedules_checked(tx)?;
        let live = states.get(contract).filter(|s| !s.cancelled).cloned();
        let at = utc(now);
        if act == "create" {
            let current = self.current_contract()?;
            if current.owner_id != contract {
                return Err(Fail::Other(format!(
                    "{contract} is not this checkout's autopilot contract (the receipt's owner id is {})",
                    if current.owner_id.is_empty() {
                        "empty"
                    } else {
                        &current.owner_id
                    }
                )));
            }
            if !APPROVED_STAGES.contains(&current.stage.as_str()) || !current.approved_run {
                return Err(Fail::Other(format!(
                    "autopilot contract at stage {} is not an approved `run --proposal FILE --expected-proposal-sha256 SHA` continuation",
                    current.stage
                )));
            }
            if live.is_some() {
                return Err(Fail::Other(format!(
                    "{contract} already has a schedule; cancel it first"
                )));
            }
            if until.is_some_and(|u| u <= now) {
                return Err(Fail::Other(format!(
                    "--until {} is not in the future",
                    utc(until.unwrap_or_default())
                )));
            }
            let anchor = now - now % 3600;
            check_tx(tx)?;
            self.append(tx, json!({"event": "create", "contract": contract, "digest": current.contract_digest, "every_hours": every, "anchor": utc(anchor), "until": until.map(utc), "at": at}))?;
            let first = schedule_occurrence(&self.schedules_checked(tx)?[contract], now).1;
            return Ok(format!(
                "scheduled {contract} every {every}h; next due {}{}\n",
                first.map_or("none".to_owned(), utc),
                until.map_or(String::new(), |u| format!("; until {}", utc(u)))
            ));
        }
        let Some(mut live) = live else {
            return Err(Fail::Other(format!("{contract} has no schedule")));
        };
        if matches!(act, "pause" | "resume") && !live.suspended {
            // No loadable receipt: nothing to compare; every fire compares again first.
            let current = self.current_contract().ok();
            let (owner, digest) = current.as_ref().map_or((None, None), |c| {
                (Some(c.owner_id.as_str()), Some(c.contract_digest.as_str()))
            });
            if schedule_view(&live, now, digest, owner) == "suspended" {
                if act == "resume" {
                    return Err(Fail::Other(format!(
                        "{contract} is suspended because its contract digest changed; review the contract, then cancel and schedule it again"
                    )));
                }
                check_tx(tx)?;
                self.append(tx, json!({"event": "skipped", "contract": contract, "reason": "digest-changed", "digest": digest, "at": at}))?;
                live.suspended = true;
            }
        }
        if act == "resume" && live.suspended {
            return Err(Fail::Other(format!(
                "{contract} is suspended because its contract digest changed; review the contract, then cancel and schedule it again"
            )));
        }
        if (act == "pause") == live.paused && act != "cancel" {
            return Ok(format!(
                "{contract} already {}\n",
                if live.paused { "paused" } else { "running" }
            ));
        }
        check_tx(tx)?;
        self.append(tx, json!({"event": act, "contract": contract, "at": at}))?;
        let word = match act {
            "pause" => "paused",
            "resume" => "resumed",
            _ => "cancelled",
        };
        Ok(format!("{word} {contract}\n"))
    }
    fn schedule_list(&self) -> std::result::Result<String, Fail> {
        let states: Vec<State> = self
            .schedules()?
            .into_values()
            .filter(|s| !s.cancelled)
            .collect();
        if states.is_empty() {
            return Ok("no schedules\n".into());
        }
        let current = self.current_contract().ok();
        let (owner, digest) = current.as_ref().map_or((None, None), |c| {
            (Some(c.owner_id.as_str()), Some(c.contract_digest.as_str()))
        });
        let mut out = String::new();
        for state in &states {
            let (due, following) = schedule_occurrence(state, self.now);
            let view = schedule_view(state, self.now, digest, owner);
            let next = match following {
                Some(f) if !["suspended", "expired"].contains(&view) => format!(
                    "{}{}",
                    utc(f),
                    if due.is_some() { " (due now)" } else { "" }
                ),
                _ => "none".into(),
            };
            let last = state.last_run.map_or("never".to_owned(), |(at, o)| {
                format!("{} (occurrence {})", utc(at), utc(o))
            });
            out.push_str(&format!(
                "{}  every {}h  state {view}  next due {next}  last run {last}  expires {}\n",
                state.contract,
                state.every,
                state.until.map_or("never".to_owned(), utc)
            ));
            if let Some(launch) = &state.launch {
                out.push_str(&format!(
                    "  launch {}  occurrence {}  {}\n",
                    launch.id,
                    utc(launch.occurrence),
                    if launch.prepared {
                        "prepared; host not called; safe to retry after revalidation"
                    } else {
                        "outcome unknown; reconcile needed; automatic retry refused"
                    }
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::PeerExecutor;
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::DirBuilderExt;

    const OWNER: &str = "owner_11111111111111111111111111111111";
    const OTHER: &str = "owner_22222222222222222222222222222222";
    const ATT: &str = "att_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MEMBER: &str = "main-1";
    const ROOM: &str = "room-1";
    const PANE: &str = "%5";

    fn at(text: &str) -> i64 {
        parse_utc(text).unwrap()
    }
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("oms-tick-{}", crate::value::unique().unwrap()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path.join(".oms"))
                .unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
        fn tick(&self, name: &str) -> PathBuf {
            self.0.join(".oms/tick").join(name)
        }
        fn put(&self, relative: &str, body: &str) {
            let path = self.0.join(relative);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(path.parent().unwrap())
                .unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
        }
        fn read(&self, relative: &str) -> String {
            std::fs::read_to_string(self.0.join(relative)).unwrap_or_default()
        }
        fn wakes(&self) -> Vec<Value> {
            let raw = self.read(".oms/tick/last.json");
            serde_json::from_str::<Value>(&raw).unwrap()["wakes"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn contract(stage: &str, owner: &str, digest: &str) -> ApprovedContract {
        let sha = "c".repeat(64);
        let argv = [
            "oms",
            "autopilot",
            "--repo",
            "/work/repo",
            "run",
            "--proposal",
            "/work/it's here",
            "--expected-proposal-sha256",
            &sha,
        ];
        ApprovedContract {
            row: json!({"stage": stage}),
            receipt_sha256: "d".repeat(64),
            contract_digest: digest.into(),
            owner_id: owner.into(),
            stage: stage.into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            approved_run: true,
        }
    }
    struct Host {
        contract: RefCell<std::result::Result<ApprovedContract, ContractError>>,
        refusal: RefCell<String>,
        stale: RefCell<String>,
        tmux: Cell<bool>,
        launched: RefCell<Vec<(Vec<String>, PathBuf)>>,
        fail_launch: Cell<bool>,
        on_load: RefCell<Option<Box<dyn Fn()>>>,
        external: RefCell<Value>,
        refreshes: Cell<usize>,
    }
    impl Host {
        fn new(stage: &str) -> Self {
            Self {
                contract: RefCell::new(Ok(contract(stage, OWNER, &"e".repeat(64)))),
                refusal: RefCell::default(),
                stale: RefCell::default(),
                tmux: Cell::new(false),
                launched: RefCell::default(),
                fail_launch: Cell::new(false),
                on_load: RefCell::default(),
                external: RefCell::new(Value::Null),
                refreshes: Cell::new(0),
            }
        }
        fn change(&self, f: impl FnOnce(&mut ApprovedContract)) {
            f(self.contract.borrow_mut().as_mut().unwrap());
        }
    }
    impl AutopilotHost for Host {
        fn external_wait(&self, _: &Path, refresh: bool) -> std::result::Result<Value, String> {
            if refresh {
                self.refreshes.set(self.refreshes.get() + 1);
            }
            Ok(self.external.borrow().clone())
        }
        fn load_contract(&self, _: &Path) -> std::result::Result<ApprovedContract, ContractError> {
            if let Some(f) = self.on_load.borrow().as_ref() {
                f();
            }
            self.contract.borrow().clone()
        }
        fn reentry_refusal(
            &self,
            _: &Path,
            _: &ApprovedContract,
        ) -> std::result::Result<String, String> {
            Ok(self.refusal.borrow().clone())
        }
        fn stale_approval(&self, _: &Path, _: &ApprovedContract, _: i64) -> String {
            self.stale.borrow().clone()
        }
        fn tmux_available(&self) -> bool {
            self.tmux.get()
        }
        fn launch(
            &self,
            argv: &[String],
            log: &Path,
            _: &Path,
        ) -> std::result::Result<u32, String> {
            if self.fail_launch.get() {
                return Err("exec refused".into());
            }
            self.launched
                .borrow_mut()
                .push((argv.to_vec(), log.to_owned()));
            Ok(4242)
        }
    }

    /// Scripted lifecycle, room, plan and tmux answers.
    struct Fake {
        repo: PathBuf,
        history: RefCell<Vec<Value>>,
        children: RefCell<Vec<Value>>,
        screen: RefCell<String>,
        sent: RefCell<Vec<Vec<String>>>,
        stamps: Cell<usize>,
        change_stamp_at: Cell<usize>,
        replace_artifacts_at_stamp: Cell<usize>,
        captures: Cell<usize>,
        change_screen_after: Cell<usize>,
        changed_screen: RefCell<String>,
        replace_artifacts_at_capture: Cell<usize>,
        fail_send_at: Cell<usize>,
        ready: RefCell<String>,
        no_session: Cell<bool>,
        main_pid: Cell<u32>,
        main_attempt: RefCell<String>,
        room_override: RefCell<Option<Value>>,
    }
    impl Fake {
        fn new(repo: &Path) -> Self {
            Self {
                repo: repo.to_owned(),
                history: RefCell::default(),
                children: RefCell::default(),
                screen: RefCell::new("╭────╮\n│ ❯ │\n╰────╯\n".into()),
                sent: RefCell::default(),
                stamps: Cell::new(0),
                change_stamp_at: Cell::new(usize::MAX),
                replace_artifacts_at_stamp: Cell::new(usize::MAX),
                captures: Cell::new(0),
                change_screen_after: Cell::new(usize::MAX),
                changed_screen: RefCell::default(),
                replace_artifacts_at_capture: Cell::new(usize::MAX),
                fail_send_at: Cell::new(usize::MAX),
                ready: RefCell::new("t1 ready\nt2 ready\n".into()),
                no_session: Cell::new(false),
                main_pid: Cell::new(4242),
                main_attempt: RefCell::new(ATT.into()),
                room_override: RefCell::default(),
            }
        }
        /// A live tmux panel main in room-1.
        fn live_main(&self) {
            self.history.borrow_mut().push(json!({"attempt_id": self.main_attempt.borrow().as_str(), "tool": "panel-main", "terminal": false, "state": "working", "provider": "claude",
                "refs": {"panel_room_id": ROOM, "panel_room_participant": MEMBER, "panel_host": "tmux", "panel_harness": "claude", "panel_role": "main"}}));
        }
        fn out(code: i32, text: impl Into<String>) -> CommandOutput {
            CommandOutput {
                code,
                stdout: text.into().into_bytes(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            }
        }
    }
    impl PeerExecutor for Fake {
        fn execute(&self, request: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            let a: Vec<String> = request
                .argv
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            if a[0] == "git" {
                // Not a work tree of its own: the given path stands.
                return Ok(Self::out(1, ""));
            }
            assert_eq!(a[0], "tmux", "only tmux is executed: {a:?}");
            let repo = self.repo.display().to_string();
            let main = self.main_attempt.borrow().clone();
            Ok(match a[1].as_str() {
                "has-session" => Self::out(i32::from(self.no_session.get()), ""),
                "show-options" => Self::out(0, format!("{repo}\n")),
                "show-environment" => Self::out(0, format!("OMS_ROOM_ID={ROOM}\n")),
                "list-windows" if a[5].starts_with("#{window_id}") => {
                    Self::out(0, format!("@2\t{main}\t{PANE}\t{MEMBER}\t{ROOM}\t{repo}\n"))
                }
                "list-windows" => {
                    Self::out(0, format!("{repo}\t{ROOM}\t{MEMBER}\tclaude\t@2\tclaude\n"))
                }
                "display-message" if a[5] == "#{pane_in_mode}" => Self::out(0, "0\n"),
                "display-message" if a[5] == "#{session_id}" => Self::out(0, "$1\n"),
                "display-message" if a[5] == crate::terminals::PROVIDER_SESSION_IDENTITY_FORMAT => {
                    Self::out(
                        0,
                        format!("$1\t{}\tfixture-socket\t.\n{repo}\n", std::process::id()),
                    )
                }
                "display-message" => {
                    let n = self.stamps.get() + 1;
                    self.stamps.set(n);
                    if n == self.replace_artifacts_at_stamp.get() {
                        let artifacts = self.repo.join(".oms/artifacts");
                        let prev = self.repo.join(".oms/artifacts-prev");
                        let _ = std::fs::remove_dir_all(&prev);
                        std::fs::rename(&artifacts, &prev).unwrap();
                        std::fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&artifacts)
                            .unwrap();
                    }
                    let attempt = if n == self.change_stamp_at.get() {
                        format!("att_{}", "b".repeat(32))
                    } else {
                        main
                    };
                    Self::out(
                        0,
                        format!("{PANE}\t{}\t0\t{attempt}\t{PANE}\n", self.main_pid.get()),
                    )
                }
                "capture-pane" => {
                    let n = self.captures.get() + 1;
                    self.captures.set(n);
                    if n == self.replace_artifacts_at_capture.get() {
                        let artifacts = self.repo.join(".oms/artifacts");
                        let prev = self.repo.join(".oms/artifacts-prev");
                        let _ = std::fs::remove_dir_all(&prev);
                        std::fs::rename(&artifacts, &prev).unwrap();
                        std::fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&artifacts)
                            .unwrap();
                    }
                    let text = if n > self.change_screen_after.get() {
                        self.changed_screen.borrow().clone()
                    } else {
                        self.screen.borrow().clone()
                    };
                    Self::out(0, text)
                }
                "send-keys" => {
                    let count = self.sent.borrow().len() + 1;
                    self.sent.borrow_mut().push(a.clone());
                    Self::out(i32::from(count == self.fail_send_at.get()), "")
                }
                other => panic!("unscripted tmux command {other}"),
            })
        }
    }
    impl Services for Fake {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            argv: &[OsString],
            _: &AtomicBool,
        ) -> Result<CommandOutput> {
            let a: Vec<String> = argv
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            Ok(match (command, a[0].as_str()) {
                (NativeCommand::Events, "list") => {
                    let mut rows = self.history.borrow().clone();
                    rows.extend(self.children.borrow().iter().cloned());
                    Self::out(0, serde_json::to_string(&rows).unwrap())
                }
                (NativeCommand::Room, "show") => Self::out(0, self.room_override.borrow().clone().unwrap_or_else(|| json!({"id": ROOM, "closed": false, "participants": [{"participant": MEMBER, "joined": true, "role": "main", "provider": "claude"}]})).to_string()),
                (NativeCommand::Plan, "ready") => Self::out(0, self.ready.borrow().clone()),
                other => panic!("unscripted command {other:?}"),
            })
        }
    }
    struct Rig {
        dir: Dir,
        host: Host,
        fake: Fake,
        stop: AtomicBool,
    }
    impl Rig {
        fn new(stage: &str) -> Self {
            let dir = Dir::new();
            let fake = Fake::new(&dir.0);
            Self {
                dir,
                host: Host::new(stage),
                fake,
                stop: AtomicBool::new(false),
            }
        }
        fn env(&self, time: &str) -> Env<'_> {
            Env {
                typing_gap: Duration::ZERO,
                ..env(&self.dir.0, at(time), &self.host, &self.fake, &self.stop)
            }
        }
        fn schedule(&self, time: &str, args: &[&str]) -> (i32, String, String) {
            self.schedule_with(time, args, false, SCHEDULE_MAX)
        }
        fn schedule_wait(
            &self,
            time: &str,
            args: &[&str],
            wait: Duration,
        ) -> (i32, String, String) {
            self.run_schedule(time, args, false, SCHEDULE_MAX, wait)
        }
        fn run_schedule(
            &self,
            time: &str,
            args: &[&str],
            child: bool,
            limit: usize,
            wait: Duration,
        ) -> (i32, String, String) {
            let argv: Vec<OsString> = ["schedule", "--repo", self.dir.0.to_str().unwrap()]
                .iter()
                .chain(args)
                .map(OsString::from)
                .collect();
            let knobs = Knobs {
                now: at(time),
                child,
                limit,
                wait,
            };
            let out = schedule_at(
                &argv,
                &self.dir.0,
                &self.host,
                &self.fake,
                &self.stop,
                &knobs,
            );
            (
                out.0,
                String::from_utf8(out.1).unwrap(),
                String::from_utf8(out.2).unwrap(),
            )
        }
        fn schedule_with(
            &self,
            time: &str,
            args: &[&str],
            child: bool,
            limit: usize,
        ) -> (i32, String, String) {
            self.run_schedule(time, args, child, limit, Duration::from_secs(5))
        }
        fn tick(&self, time: &str, scheduled: bool, marker: Option<&Path>) -> String {
            self.env(time).resume(false, marker, scheduled)
        }
        fn log(&self) -> String {
            self.dir.read(".oms/tick/schedules.jsonl")
        }
        fn seed(&self, rows: Value) {
            self.dir.put(
                ".oms/tick/last.json",
                &json!({"schema": 1, "wakes": rows}).to_string(),
            );
        }
        /// The idle record the activity hook writes for the verified main.
        fn idle_since(&self, time: &str, seconds_before: i64) {
            self.dir.put(
                &format!(".oms/hooks/panel-activity/{MEMBER}.json"),
                &json!({"state": "idle", "attempt": *self.fake.main_attempt.borrow(), "since": at(time) - seconds_before})
                    .to_string(),
            );
        }
    }
    fn wake_row(day: &str, main: &str, key: &str, outcome: &str) -> Value {
        json!({"day": day, "main": main, "key": key, "outcome": outcome})
    }

    // ---- schedule math and state

    fn state(every: i64, anchor: &str, floor: Option<&str>, until: Option<&str>) -> State {
        State {
            contract: OWNER.into(),
            digest: "e".repeat(64),
            every,
            anchor: at(anchor),
            paused: false,
            until: until.map(at),
            floor: floor.map_or(at(anchor), at),
            last_run: None,
            launch: None,
            suspended: false,
            cancelled: false,
        }
    }
    #[test]
    fn missed_occurrences_coalesce_into_the_latest_due_one() {
        let s = state(2, "2026-11-09T21:00:00Z", None, None);
        assert_eq!(
            schedule_occurrence(&s, at("2026-11-09T22:59:59Z")),
            (None, Some(at("2026-11-09T23:00:00Z")))
        );
        assert_eq!(
            schedule_occurrence(&s, at("2026-11-09T23:00:00Z")),
            (
                Some(at("2026-11-09T23:00:00Z")),
                Some(at("2026-11-09T23:00:00Z"))
            )
        );
        // Three whole steps missed: exactly one run, at the latest occurrence.
        assert_eq!(
            schedule_occurrence(&s, at("2026-11-10T04:30:00Z")).0,
            Some(at("2026-11-10T03:00:00Z"))
        );
        let fired = state(
            2,
            "2026-11-09T21:00:00Z",
            Some("2026-11-10T03:00:00Z"),
            None,
        );
        assert_eq!(
            schedule_occurrence(&fired, at("2026-11-10T04:30:00Z")),
            (None, Some(at("2026-11-10T05:00:00Z")))
        );
        let ending = state(
            1,
            "2026-11-10T18:00:00Z",
            None,
            Some("2026-11-10T20:00:00Z"),
        );
        assert_eq!(
            schedule_occurrence(&ending, at("2026-11-10T18:30:00Z")).1,
            Some(at("2026-11-10T19:00:00Z"))
        );
        assert_eq!(
            schedule_occurrence(&ending, at("2026-11-10T19:30:00Z")),
            (
                Some(at("2026-11-10T19:00:00Z")),
                Some(at("2026-11-10T19:00:00Z"))
            )
        );
        assert_eq!(
            schedule_occurrence(&ending, at("2026-11-10T20:00:00Z")),
            (None, None),
            "no occurrence at or after the expiry"
        );
        let last = state(
            1,
            "2026-11-10T18:00:00Z",
            Some("2026-11-10T19:00:00Z"),
            Some("2026-11-10T20:00:00Z"),
        );
        assert_eq!(
            schedule_occurrence(&last, at("2026-11-10T19:30:00Z")),
            (None, None),
            "the next one would be at the expiry"
        );
    }
    #[test]
    fn schedule_view_orders_suspended_expired_paused_active() {
        let mut s = state(
            2,
            "2026-11-09T21:00:00Z",
            None,
            Some("2026-11-12T00:00:00Z"),
        );
        let now = at("2026-11-10T00:00:00Z");
        assert_eq!(
            schedule_view(&s, now, Some(&"e".repeat(64)), Some(OWNER)),
            "active"
        );
        assert_eq!(
            schedule_view(&s, now, Some(&"e".repeat(64)), Some(OTHER)),
            "inactive"
        );
        assert_eq!(
            schedule_view(&s, now, Some(&"f".repeat(64)), Some(OWNER)),
            "suspended"
        );
        assert_eq!(
            schedule_view(&s, now, None, None),
            "inactive",
            "no loadable receipt: nothing to compare"
        );
        s.paused = true;
        assert_eq!(
            schedule_view(&s, now, Some(&"e".repeat(64)), Some(OWNER)),
            "paused"
        );
        assert_eq!(
            schedule_view(
                &s,
                at("2026-11-12T00:00:00Z"),
                Some(&"e".repeat(64)),
                Some(OWNER)
            ),
            "expired"
        );
        s.suspended = true;
        assert_eq!(
            schedule_view(&s, now, Some(&"e".repeat(64)), Some(OWNER)),
            "suspended"
        );
    }

    // ---- `oms tick schedule`

    #[test]
    fn schedule_events_create_pause_resume_cancel_and_list() {
        let rig = Rig::new("driving");
        let t = "2026-11-09T21:10:00Z";
        assert_eq!(
            rig.schedule(t, &["--list"]),
            (0, "no schedules\n".into(), String::new())
        );
        assert!(
            !rig.dir.tick("schedules.jsonl").exists(),
            "a list never creates the log"
        );
        let made = rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        assert_eq!(
            made,
            (
                0,
                format!("scheduled {OWNER} every 2h; next due 2026-11-09T23:00:00Z\n"),
                String::new()
            )
        );
        assert_eq!(
            rig.log(),
            format!(
                "{{\"anchor\":\"2026-11-09T21:00:00Z\",\"at\":\"2026-11-09T21:10:00Z\",\"contract\":\"{OWNER}\",\"digest\":\"{}\",\"event\":\"create\",\"every_hours\":2,\"schema\":1,\"until\":null}}\n",
                "e".repeat(64)
            )
        );
        let listed = rig.schedule(t, &["--list"]).1;
        assert_eq!(
            listed,
            format!(
                "{OWNER}  every 2h  state active  next due 2026-11-09T23:00:00Z  last run never  expires never\n"
            )
        );
        let dup = rig.schedule(t, &["--contract", OWNER, "--every", "3"]);
        assert!(
            dup.0 == 1 && dup.2.contains("already has a schedule; cancel it first"),
            "{dup:?}"
        );
        assert_eq!(
            rig.schedule(t, &["--pause", "--contract", OWNER]).1,
            format!("paused {OWNER}\n")
        );
        assert_eq!(
            rig.schedule(t, &["--pause", "--contract", OWNER]).1,
            format!("{OWNER} already paused\n")
        );
        assert!(rig.schedule(t, &["--list"]).1.contains("state paused"));
        assert_eq!(
            rig.schedule(t, &["--resume", "--contract", OWNER]).1,
            format!("resumed {OWNER}\n")
        );
        assert_eq!(
            rig.schedule(t, &["--resume", "--contract", OWNER]).1,
            format!("{OWNER} already running\n")
        );
        assert_eq!(
            rig.schedule(t, &["--cancel", "--contract", OWNER]).1,
            format!("cancelled {OWNER}\n")
        );
        let gone = rig.schedule(t, &["--cancel", "--contract", OWNER]);
        assert_eq!(
            (gone.0, gone.2.as_str()),
            (1, format!("error: {OWNER} has no schedule\n").as_str())
        );
        assert_eq!(rig.schedule(t, &["--list"]).1, "no schedules\n");
        assert!(
            !schedules_pending(&rig.dir.0, Some(at(t))),
            "a log of cancelled schedules is no schedule"
        );
        // Cancel retires it so a new one binds, with an expiry this time.
        let again = rig.schedule(
            t,
            &[
                "--contract",
                OWNER,
                "--every",
                "1",
                "--until",
                "2026-11-10T00:00:00Z",
            ],
        );
        assert_eq!(
            again.1,
            format!(
                "scheduled {OWNER} every 1h; next due 2026-11-09T22:00:00Z; until 2026-11-10T00:00:00Z\n"
            )
        );
        assert!(
            schedules_pending(&rig.dir.0, Some(at(t)))
                && !schedules_pending(&rig.dir.0, Some(at("2026-11-10T00:00:00Z"))),
            "pending ends at the expiry"
        );
    }
    #[test]
    fn schedule_binds_only_this_checkouts_approved_future_contract() {
        let rig = Rig::new("driving");
        let t = "2026-11-09T21:10:00Z";
        let wrong = rig.schedule(t, &["--contract", OTHER, "--every", "2"]);
        assert!(
            wrong.0 == 1
                && wrong
                    .2
                    .contains("is not this checkout's autopilot contract"),
            "{wrong:?}"
        );
        for until in ["2026-11-09", "2026-11-09T21:10Z"] {
            let past = rig.schedule(t, &["--contract", OWNER, "--every", "2", "--until", until]);
            assert!(
                past.0 == 1 && past.2.contains("is not in the future"),
                "{until}: {past:?}"
            );
        }
        for stage in ["proposal-review", "parked", "done", "proposing"] {
            rig.host.change(|c| c.stage = stage.into());
            let refused = rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
            assert!(
                refused.0 == 1
                    && refused.2.contains(
                        "is not an approved `run --proposal FILE --expected-proposal-sha256 SHA`"
                    ),
                "{stage}: {refused:?}"
            );
        }
        rig.host.change(|c| {
            c.stage = "approved".into();
            c.approved_run = false;
        });
        assert_eq!(
            rig.schedule(t, &["--contract", OWNER, "--every", "2"]).0,
            1,
            "a continuation that is not `run --proposal` never binds"
        );
        *rig.host.contract.borrow_mut() = Err(ContractError::Missing);
        assert!(
            rig.schedule(t, &["--contract", OWNER, "--every", "2"])
                .2
                .contains("autopilot receipt unavailable")
        );
        assert_eq!(rig.log(), "", "every refusal wrote nothing");
        // The limit itself binds.
        *rig.host.contract.borrow_mut() = Ok(contract("driving", OWNER, &"e".repeat(64)));
        assert!(
            rig.schedule(t, &["--contract", OWNER, "--every", "8760"])
                .1
                .contains("next due 2027-11-09T21:00:00Z")
        );
    }
    #[test]
    fn malformed_schedule_requests_exit_2_and_write_nothing() {
        let rig = Rig::new("driving");
        let t = "2026-11-09T21:10:00Z";
        for every in ["0", "-1", "1.5", "", "abc", "8761", "123456", "+2"] {
            assert_eq!(
                rig.schedule(t, &["--contract", OWNER, "--every", every]).0,
                2,
                "--every {every:?}"
            );
        }
        let bad: [&[&str]; 10] = [
            &["--contract", "owner_xyz", "--every", "2"],
            &["--every", "2"],
            &[],
            &["--list", "--contract", OWNER],
            &["--list", "--every", "2"],
            &["--pause", "--every", "2", "--contract", OWNER],
            &["--pause", "--cancel", "--contract", OWNER],
            &["--contract", OWNER, "--every", "2", "--until", "2026/12/01"],
            &["--contract", OWNER, "--every"],
            &["--bogus"],
        ];
        for argv in bad {
            let out = rig.schedule(t, argv);
            assert!(
                out.0 == 2 && out.2.contains("oms tick schedule: error:"),
                "{argv:?}: {out:?}"
            );
        }
        assert!(!rig.dir.tick("schedules.jsonl").exists());
        // A harness child cannot schedule; reading stays allowed.
        let child = rig.schedule_with(
            t,
            &["--contract", OWNER, "--every", "2"],
            true,
            SCHEDULE_MAX,
        );
        assert!(
            child.0 == 2 && child.2.contains("harness child cannot schedule"),
            "{child:?}"
        );
        assert_eq!(rig.schedule_with(t, &["--list"], true, SCHEDULE_MAX).0, 0);
        assert!(!rig.dir.tick("schedules.jsonl").exists());
        // Equals form and a repository that was never adopted.
        let equals = Rig::new("driving");
        let knobs = Knobs {
            now: at(t),
            child: false,
            limit: SCHEDULE_MAX,
            wait: Duration::from_secs(5),
        };
        let argv = [
            OsString::from(format!("--repo={}", equals.dir.0.display())),
            "--list".into(),
        ];
        assert_eq!(
            schedule_at(
                &argv,
                &equals.dir.0,
                &equals.host,
                &equals.fake,
                &equals.stop,
                &knobs
            )
            .0,
            0
        );
        std::fs::remove_dir(equals.dir.0.join(".oms")).unwrap();
        assert_eq!(
            schedule_at(
                &argv,
                &equals.dir.0,
                &equals.host,
                &equals.fake,
                &equals.stop,
                &knobs
            )
            .0,
            2
        );
    }
    #[test]
    fn a_changed_contract_digest_suspends_the_schedule_for_good() {
        let rig = Rig::new("driving");
        let t = "2026-11-09T21:10:00Z";
        rig.schedule(t, &["--contract", OWNER, "--every", "1"]);
        let due = "2026-11-09T22:30:00Z";
        rig.host.change(|c| c.contract_digest = "f".repeat(64));
        let before_resume = rig.log();
        let suspended_resume = rig.schedule(due, &["--resume", "--contract", OWNER]);
        assert!(
            suspended_resume.0 == 1
                && suspended_resume
                    .2
                    .contains("is suspended because its contract digest changed"),
            "{suspended_resume:?}"
        );
        assert_eq!(rig.log(), before_resume, "refused --resume must not mutate");
        let list = rig.schedule(due, &["--list"]).1;
        assert!(
            list.contains("state suspended") && list.contains("next due none"),
            "{list}"
        );
        assert!(rig.tick(due, true, None).starts_with(&format!(
            "skipped: schedule for {OWNER} suspended: the contract digest changed"
        )));
        assert!(rig.host.launched.borrow().is_empty());
        let events: Vec<Value> = rig
            .log()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| e["reason"] == "digest-changed")
                .count(),
            1,
            "recorded once"
        );
        rig.tick(due, true, None);
        assert_eq!(
            rig.log().lines().count(),
            2,
            "the second skip appends nothing"
        );
        // Sticky even if the contract changes back; resume refuses re-approval, pause is a no-op write.
        rig.host.change(|c| c.contract_digest = "e".repeat(64));
        assert!(rig.schedule(due, &["--list"]).1.contains("state suspended"));
        let resume = rig.schedule(due, &["--resume", "--contract", OWNER]);
        assert!(
            resume.0 == 1
                && resume
                    .2
                    .contains("is suspended because its contract digest changed"),
            "{resume:?}"
        );
        assert_eq!(rig.schedule(due, &["--cancel", "--contract", OWNER]).0, 0);
        assert!(
            rig.schedule(due, &["--contract", OWNER, "--every", "1"])
                .1
                .starts_with("scheduled"),
            "cancel then schedule again"
        );
    }
    #[test]
    fn pause_on_a_changed_digest_records_the_suspension_first() {
        let rig = Rig::new("driving");
        rig.schedule(
            "2026-11-09T21:10:00Z",
            &["--contract", OWNER, "--every", "1"],
        );
        rig.host.change(|c| c.contract_digest = "f".repeat(64));
        assert!(
            rig.schedule("2026-11-09T21:20:00Z", &["--pause", "--contract", OWNER])
                .0
                == 0
        );
        let kinds: Vec<String> = rig
            .log()
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(kinds, ["create", "skipped", "pause"]);
    }

    // ---- fail-closed schedule log

    #[test]
    fn a_corrupt_schedule_log_refuses_every_path_and_is_kept_as_found() {
        let rig = Rig::new("driving");
        let t = "2026-11-09T21:10:00Z";
        rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let good = rig.log();
        let bad_create = json!({"schema": 1, "event": "create", "contract": OTHER, "digest": "0".repeat(64), "every_hours": 0, "anchor": "2026-11-10T00:00:00Z", "at": "2026-11-10T00:00:00Z"});
        let cases = [
            "".to_owned(),
            good[..good.len() - 1].to_owned(),
            format!("{good}{{\n"),
            format!("{good}\n"),
            format!("{good}{bad_create}\n"),
            good.replace("\"schema\":1", "\"schema\":2"),
            good.replace(OWNER, "owner_xyz"),
            good.replace("\"create\"", "\"bogus\""),
            format!(
                "{good}{}\n",
                json!({"schema": 1, "event": "pause", "contract": OTHER, "at": "2026-11-10T00:00:00Z"})
            ),
        ];
        for (n, bad) in cases.iter().enumerate() {
            rig.dir.put(".oms/tick/schedules.jsonl", bad);
            for scheduled in [true, false] {
                let status = rig.tick(t, scheduled, None);
                assert!(
                    status.starts_with("refused: schedule log ")
                        && status.contains("kept as found"),
                    "case {n}: {status}"
                );
            }
            assert_eq!(rig.schedule(t, &["--list"]).0, 1, "case {n}");
            assert_eq!(
                rig.schedule(t, &["--pause", "--contract", OWNER]).0,
                1,
                "case {n}"
            );
            assert!(schedules_pending(&rig.dir.0, Some(at(t))), "case {n}");
            assert_eq!(
                rig.log(),
                *bad,
                "case {n}: the file must stay byte for byte"
            );
        }
        assert!(rig.host.launched.borrow().is_empty());
    }
    #[test]
    fn a_linked_schedule_log_refuses() {
        let rig = Rig::new("driving");
        let t = "2026-11-09T21:10:00Z";
        rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let good = rig.log();
        std::fs::remove_file(rig.dir.tick("schedules.jsonl")).unwrap();
        rig.dir.put(".oms/tick/elsewhere.jsonl", &good);
        std::os::unix::fs::symlink("elsewhere.jsonl", rig.dir.tick("schedules.jsonl")).unwrap();
        for scheduled in [true, false] {
            assert!(
                rig.tick(t, scheduled, None)
                    .starts_with("refused: schedule log is a symlink")
            );
        }
        assert_eq!(rig.schedule(t, &["--pause", "--contract", OWNER]).0, 1);
        assert_eq!(
            rig.dir.read(".oms/tick/elsewhere.jsonl"),
            good,
            "the link target was not appended to"
        );
        std::fs::remove_file(rig.dir.tick("schedules.jsonl")).unwrap();
        std::fs::hard_link(
            rig.dir.tick("elsewhere.jsonl"),
            rig.dir.tick("schedules.jsonl"),
        )
        .unwrap();
        assert!(
            rig.tick(t, false, None)
                .starts_with("refused: schedule log is a symlink, a hard link")
        );
        assert_eq!(rig.schedule(t, &["--cancel", "--contract", OWNER]).0, 1);
        assert_eq!(rig.dir.read(".oms/tick/elsewhere.jsonl"), good);

        // Linked .oms ancestor pointing outside (with and without an existing tick subdir) refuses without touching outside.
        let outside = Dir::new();
        outside.put("tick/schedules.jsonl", &good);
        std::fs::remove_dir_all(rig.dir.0.join(".oms")).unwrap();
        std::os::unix::fs::symlink(&outside.0, rig.dir.0.join(".oms")).unwrap();
        for scheduled in [true, false] {
            assert!(rig.tick(t, scheduled, None).starts_with("refused:"));
        }
        assert_eq!(rig.schedule(t, &["--contract", OWNER, "--every", "2"]).0, 1);
        assert_eq!(outside.read("tick/schedules.jsonl"), good);

        std::fs::remove_dir_all(outside.0.join("tick")).unwrap();
        for scheduled in [true, false] {
            assert!(rig.tick(t, scheduled, None).starts_with("refused:"));
        }
        assert_eq!(rig.schedule(t, &["--contract", OWNER, "--every", "2"]).0, 1);
        assert!(!outside.0.join("tick").exists());

        // Controlled .oms symlink replacement between tick validation and panel lock acquisition
        // must refuse without creating outside/artifacts/panel-main-start.lock.
        let race_rig = Rig::new("driving");
        race_rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let race_good = race_rig.log();
        let outside_race = Dir::new();
        outside_race.put("tick/schedules.jsonl", &race_good);
        std::fs::create_dir_all(outside_race.0.join("artifacts")).unwrap();
        let oms = race_rig.dir.0.join(".oms");
        let hook = || {
            std::fs::remove_dir_all(&oms).unwrap();
            std::os::unix::fs::symlink(&outside_race.0, &oms).unwrap();
        };
        let mut race_env = race_rig.env(t);
        race_env.before_panel_lock = Some(&hook);
        let out = race_env.resume(false, None, false);
        assert!(out.starts_with("refused:"), "{out}");
        assert!(
            std::fs::read_dir(outside_race.0.join("artifacts"))
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(outside_race.read("tick/schedules.jsonl"), race_good);

        // Replacement of .oms/artifacts after panel-lock acquisition must refuse before launching or writing.
        let post_rig = Rig::new("driving");
        post_rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let post_good = post_rig.log();
        let artifacts = post_rig.dir.0.join(".oms/artifacts");
        let moved_artifacts = post_rig.dir.0.join(".oms/artifacts-prev");
        std::fs::create_dir_all(&artifacts).unwrap();
        let post_hook = || {
            std::fs::rename(&artifacts, &moved_artifacts).unwrap();
            std::fs::create_dir_all(&artifacts).unwrap();
        };
        let mut post_env = post_rig.env(t);
        post_env.after_panel_lock = Some(&post_hook);
        let post_out = post_env.resume(false, None, false);
        assert!(post_out.starts_with("refused:"), "{post_out}");
        assert!(post_rig.host.launched.borrow().is_empty());
        assert!(
            std::fs::read_dir(&artifacts).unwrap().next().is_none(),
            "replacement artifacts directory stayed untouched"
        );
        assert_eq!(post_rig.log(), post_good);
        assert!(!post_rig.dir.0.join(".oms/tick/last.json").exists());

        // Replacing .oms/artifacts with a symlink after panel-lock acquisition also refuses cleanly.
        let sym_rig = Rig::new("driving");
        sym_rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let sym_good = sym_rig.log();
        let outside_post = Dir::new();
        let trap = outside_post.0.join("trap");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&trap)
            .unwrap();
        let sym_artifacts = sym_rig.dir.0.join(".oms/artifacts");
        std::fs::create_dir_all(&sym_artifacts).unwrap();
        let sym_hook = || {
            std::fs::remove_dir_all(&sym_artifacts).unwrap();
            std::os::unix::fs::symlink(&trap, &sym_artifacts).unwrap();
        };
        let mut sym_env = sym_rig.env(t);
        sym_env.after_panel_lock = Some(&sym_hook);
        let sym_out = sym_env.resume(false, None, false);
        assert!(sym_out.starts_with("refused:"), "{sym_out}");
        assert!(sym_rig.host.launched.borrow().is_empty());
        assert!(std::fs::read_dir(&trap).unwrap().next().is_none());
        assert_eq!(sym_rig.log(), sym_good);
        assert!(!sym_rig.dir.0.join(".oms/tick/last.json").exists());

        // Direct Transaction operations also refuse once .oms/artifacts is replaced.
        let tx_rig = Rig::new("driving");
        tx_rig.dir.put(".oms/tick/sample.txt", "before\n");
        let store = Store::open(&tx_rig.dir.0).unwrap();
        let tx = store
            .transaction("panel-main-start", Duration::from_secs(1))
            .unwrap();
        let tx_artifacts = tx_rig.dir.0.join(".oms/artifacts");
        std::fs::rename(&tx_artifacts, tx_rig.dir.0.join(".oms/artifacts-prev")).unwrap();
        std::fs::create_dir_all(&tx_artifacts).unwrap();
        assert!(tx.check().is_err());
        assert!(
            tx.read(Path::new(".oms/tick/sample.txt"), 64, false)
                .is_err()
        );
        assert!(
            tx.write(Path::new(".oms/tick/sample.txt"), b"after\n", false, 64)
                .is_err()
        );
        assert_eq!(tx_rig.dir.read(".oms/tick/sample.txt"), "before\n");

        // Schedule create/pause/resume/cancel also refuse before appending if .oms/artifacts is replaced.
        let sched_pause_rig = Rig::new("driving");
        sched_pause_rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let pause_before = sched_pause_rig.log();
        let sp_artifacts = sched_pause_rig.dir.0.join(".oms/artifacts");
        let sp_prev = sched_pause_rig.dir.0.join(".oms/artifacts-prev");
        std::fs::create_dir_all(&sp_artifacts).unwrap();
        let sp_hook = || {
            std::fs::rename(&sp_artifacts, &sp_prev).unwrap();
            std::fs::create_dir_all(&sp_artifacts).unwrap();
        };
        let mut sp_env = sched_pause_rig.env(t);
        sp_env.after_panel_lock = Some(&sp_hook);
        assert!(matches!(
            sp_env.locked(Duration::from_millis(500), |tx| {
                sp_env.schedule_change(Some(tx), "pause", OWNER, 2, None)
            }),
            Err(Fail::Ledger(_))
        ));
        assert_eq!(sched_pause_rig.log(), pause_before);

        let sched_create_rig = Rig::new("driving");
        let sc_artifacts = sched_create_rig.dir.0.join(".oms/artifacts");
        let sc_prev = sched_create_rig.dir.0.join(".oms/artifacts-prev");
        std::fs::create_dir_all(&sc_artifacts).unwrap();
        let sc_hook = || {
            std::fs::rename(&sc_artifacts, &sc_prev).unwrap();
            std::fs::create_dir_all(&sc_artifacts).unwrap();
        };
        let mut sc_env = sched_create_rig.env(t);
        sc_env.after_panel_lock = Some(&sc_hook);
        assert!(matches!(
            sc_env.locked(Duration::from_millis(500), |tx| {
                sc_env.schedule_change(Some(tx), "create", OWNER, 2, None)
            }),
            Err(Fail::Ledger(_))
        ));
        assert!(sched_create_rig.log().is_empty());

        // Replacement of .oms/tick after panel-lock acquisition must refuse before launching or writing,
        // preserving the existing ledger in the original directory.
        let tick_swap = Rig::new("driving");
        let ledger_before = "{\"wakes\":[{\"day\":\"2026-11-09\",\"main\":\"autopilot\",\"key\":\"k0\",\"outcome\":\"woke\"}]}";
        tick_swap.dir.put(".oms/tick/last.json", ledger_before);
        tick_swap.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let sched_before = tick_swap.log();
        let tick_dir = tick_swap.dir.0.join(".oms/tick");
        let tick_prev = tick_swap.dir.0.join(".oms/tick-prev");
        let swap_hook = || {
            std::fs::rename(&tick_dir, &tick_prev).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&tick_dir)
                .unwrap();
        };
        let mut tick_env = tick_swap.env(t);
        tick_env.after_panel_lock = Some(&swap_hook);
        let tick_out = tick_env.resume(false, None, false);
        assert!(tick_out.starts_with("refused:"), "{tick_out}");
        assert!(tick_swap.host.launched.borrow().is_empty());
        assert!(
            std::fs::read_dir(&tick_dir).unwrap().next().is_none(),
            "replacement tick directory stays empty"
        );
        assert_eq!(
            tick_swap.dir.read(".oms/tick-prev/last.json"),
            ledger_before
        );
        assert_eq!(
            tick_swap.dir.read(".oms/tick-prev/schedules.jsonl"),
            sched_before
        );

        // Replacement of .oms/tick AFTER the initial lock/read comparison (during host contract loading)
        // must also refuse before appending fired, writing last.json, or launching.
        let mid_swap = Rig::new("driving");
        mid_swap.dir.put(".oms/tick/last.json", ledger_before);
        mid_swap.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let mid_sched_before = mid_swap.log();
        let mid_dir = mid_swap.dir.0.join(".oms/tick");
        let mid_prev = mid_swap.dir.0.join(".oms/tick-prev");
        *mid_swap.host.on_load.borrow_mut() = Some(Box::new(move || {
            if !mid_prev.exists() {
                std::fs::rename(&mid_dir, &mid_prev).unwrap();
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&mid_dir)
                    .unwrap();
            }
        }));
        let mid_out = mid_swap
            .env("2026-11-09T23:10:00Z")
            .resume(false, None, false);
        assert!(mid_out.starts_with("refused:"), "{mid_out}");
        assert!(mid_swap.host.launched.borrow().is_empty());
        assert!(
            std::fs::read_dir(mid_swap.dir.0.join(".oms/tick"))
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(mid_swap.dir.read(".oms/tick-prev/last.json"), ledger_before);
        assert_eq!(
            mid_swap.dir.read(".oms/tick-prev/schedules.jsonl"),
            mid_sched_before
        );
    }
    #[test]
    fn the_schedule_size_cap_is_exact_for_reads_and_appends() {
        let rig = Rig::new("driving");
        let t = "2026-11-10T10:00:00Z";
        rig.schedule(t, &["--contract", OWNER, "--every", "2"]);
        let good = rig.log();
        let pause = format!(
            "{{\"at\":\"{}\",\"contract\":\"{OWNER}\",\"event\":\"pause\",\"schema\":1}}\n",
            utc(at(t))
        );
        // A log at the limit reads; one byte over refuses and is kept.
        assert_eq!(rig.schedule_with(t, &["--list"], false, good.len()).0, 0);
        rig.dir
            .put(".oms/tick/schedules.jsonl", &format!("{good}\n"));
        let big = rig.schedule_with(t, &["--list"], false, good.len());
        assert!(big.0 == 1 && big.2.contains("too large"), "{big:?}");
        assert_eq!(rig.log(), format!("{good}\n"));
        // An append one byte past the cap is refused before it writes; ending exactly at the cap lands.
        rig.dir.put(".oms/tick/schedules.jsonl", &good);
        let over = rig.schedule_with(
            t,
            &["--pause", "--contract", OWNER],
            false,
            good.len() + pause.len() - 1,
        );
        assert!(over.0 == 1 && over.2.contains("would exceed"), "{over:?}");
        assert_eq!(rig.log(), good);
        assert_eq!(
            rig.schedule_with(
                t,
                &["--pause", "--contract", OWNER],
                false,
                good.len() + pause.len()
            )
            .0,
            0
        );
        assert_eq!(rig.log(), format!("{good}{pause}"));
    }

    // ---- wake ledger and the resume path

    #[test]
    fn an_unscheduled_resume_records_before_it_spawns_and_dedupes_by_receipt_state() {
        let rig = Rig::new("driving");
        rig.dir.put(".oms/tick/last.json", &json!({"schema": 1, "ran_at": "x", "wakes": [wake_row("2026-11-10", "call-other", "k0", "woke"), wake_row("2026-11-09", "call-old", "k9", "woke")]}).to_string());
        let t = "2026-11-10T09:00:00Z";
        let status = rig.tick(t, false, None);
        let short = "d".repeat(12);
        assert_eq!(
            status,
            format!(
                "resumed autopilot (stage driving, receipt {short}, pid 4242, output .oms/tick/autopilot-{short}.log): oms autopilot --repo /work/repo run --proposal '/work/it'\"'\"'s here' --expected-proposal-sha256 {}",
                "c".repeat(64)
            )
        );
        let launched = rig.host.launched.borrow().clone();
        assert_eq!(launched.len(), 1);
        assert_eq!(
            launched[0].1,
            rig.dir.0.join(format!(".oms/tick/autopilot-{short}.log"))
        );
        let wakes = rig.dir.wakes();
        assert_eq!(
            wakes.len(),
            2,
            "today's rows are kept and one is added; an older day is pruned"
        );
        assert_eq!(wakes[0], wake_row("2026-11-10", "call-other", "k0", "woke"));
        assert_eq!(
            (
                wakes[1]["main"].as_str(),
                wakes[1]["outcome"].as_str(),
                wakes[1]["stage"].as_str()
            ),
            (Some("autopilot"), Some("resumed"), Some("driving"))
        );
        assert_eq!(
            wakes[1]["key"],
            hash16(&["autopilot", "2026-11-10", &"d".repeat(64)])
        );
        assert_eq!(
            serde_json::from_str::<Value>(&rig.dir.read(".oms/tick/last.json")).unwrap()["ran_at"],
            "x",
            "other receipt fields survive"
        );
        let again = rig.tick("2026-11-10T10:00:00Z", false, None);
        assert_eq!(
            again,
            "skipped: autopilot already resumed today at this receipt state; resumes when the receipt changes or the UTC day rolls over"
        );
        rig.host.change(|c| c.receipt_sha256 = "9".repeat(64));
        assert!(
            rig.tick("2026-11-10T10:00:00Z", false, None)
                .starts_with("resumed autopilot"),
            "a moved receipt is a new key"
        );
        assert!(
            rig.tick("2026-11-11T00:00:00Z", false, None)
                .starts_with("resumed autopilot"),
            "the next UTC day is new"
        );
        assert_eq!(rig.host.launched.borrow().len(), 3);
    }
    #[test]
    fn the_daily_wake_budget_counts_spent_rows_only_and_is_exact() {
        let rig = Rig::new("driving");
        let day = "2026-11-10";
        let spent = |n: usize| {
            json!(
                (0..n)
                    .map(|i| wake_row(day, "call-other", &format!("k{i}"), "woke"))
                    .collect::<Vec<_>>()
            )
        };
        rig.seed(spent(WAKE_DAILY_LIMIT));
        assert_eq!(
            rig.tick("2026-11-10T09:00:00Z", false, None),
            "skipped: 6 wakes already spent today; resumes when the UTC day rolls over"
        );
        assert!(rig.host.launched.borrow().is_empty());
        rig.seed(spent(WAKE_DAILY_LIMIT - 1));
        assert!(
            rig.tick("2026-11-10T09:00:00Z", false, None)
                .starts_with("resumed autopilot"),
            "five spent leaves one"
        );
        assert_eq!(rig.dir.wakes().len(), WAKE_DAILY_LIMIT);
        // Skipped rows spend nothing.
        let mut rows = spent(0);
        rows.as_array_mut()
            .unwrap()
            .extend((0..9).map(|i| wake_row(day, "call-other", &format!("s{i}"), "skipped")));
        rig.seed(rows);
        assert!(
            rig.tick("2026-11-10T09:00:00Z", false, None)
                .starts_with("resumed autopilot")
        );
    }
    #[test]
    fn a_stale_approval_is_recorded_once_per_receipt_state_and_day() {
        let rig = Rig::new("driving");
        *rig.host.stale.borrow_mut() = "autopilot approval is 9d old (approved 2026-11-01T00:00:00Z by a person's run; limit 7d)".into();
        let first = rig.tick("2026-11-10T09:00:00Z", false, None);
        assert!(
            first.starts_with("skipped: autopilot approval is 9d old")
                && first.contains("`oms autopilot --repo ")
                && first.contains("reenter"),
            "{first}"
        );
        rig.tick("2026-11-10T09:30:00Z", false, None);
        let wakes = rig.dir.wakes();
        assert_eq!(wakes.len(), 1, "one recorded skip");
        assert_eq!(
            (wakes[0]["outcome"].as_str(), wakes[0]["main"].as_str()),
            (Some("skipped"), Some("autopilot"))
        );
        assert_eq!(
            wakes[0]["key"],
            hash16(&["autopilot", "stale-approval", "2026-11-10", &"d".repeat(64)])
        );
        assert!(rig.host.launched.borrow().is_empty());
        // Once re-approved, an earlier recorded skip neither blocks nor spends the budget.
        rig.host.stale.borrow_mut().clear();
        assert!(
            rig.tick("2026-11-10T10:00:00Z", false, None)
                .starts_with("resumed autopilot")
        );
        assert_eq!(rig.dir.wakes().len(), 2);
    }
    #[test]
    fn the_per_run_marker_lets_one_tick_run_resume_once() {
        let rig = Rig::new("driving");
        let marker = rig.dir.0.join("resume-marker");
        assert!(
            rig.tick("2026-11-10T09:00:00Z", false, Some(&marker))
                .starts_with("resumed autopilot")
        );
        assert!(marker.exists());
        rig.host.change(|c| c.receipt_sha256 = "9".repeat(64));
        assert_eq!(
            rig.tick("2026-11-10T09:00:00Z", false, Some(&marker)),
            "skipped: this tick run already resumed one autopilot; resumes when the next tick run"
        );
        let bad = rig.dir.0.join("no-such-dir/marker");
        assert!(
            rig.tick("2026-11-10T09:00:00Z", false, Some(&bad))
                .starts_with("skipped: cannot claim this tick run's resume marker")
        );
        assert_eq!(rig.host.launched.borrow().len(), 1);
    }
    #[test]
    fn resume_reports_why_it_declines() {
        let rig = Rig::new("driving");
        let t = "2026-11-10T09:00:00Z";
        *rig.host.contract.borrow_mut() = Err(ContractError::Missing);
        assert_eq!(
            rig.tick(t, false, None),
            "skipped: no live main and no autopilot contract; resumes when `oms autopilot propose` and a reviewed `run --proposal` bind one"
        );
        *rig.host.contract.borrow_mut() = Err(ContractError::Invalid(
            "outer receipt is not an object".into(),
        ));
        assert!(rig.tick(t, false, None).starts_with("skipped: autopilot receipt invalid (outer receipt is not an object); resumes when `oms autopilot --repo "));
        *rig.host.contract.borrow_mut() = Ok(contract("parked", OWNER, &"e".repeat(64)));
        rig.host.change(|c| c.row = json!({"stage": "parked", "diagnostic": {"reason": "inspect the wall", "reason_code": "wall-kill"}}));
        *rig.host.refusal.borrow_mut() = "the run is parked for an operator decision".into();
        assert_eq!(
            rig.tick(t, false, None),
            "skipped: autopilot parked: the run is parked for an operator decision; next step: inspect the wall (wall-kill); resumes when the operator resolves the park (`oms autopilot --repo . status`) or abandons it"
        );
        *rig.host.contract.borrow_mut() = Ok(contract("proposal-review", OWNER, &"e".repeat(64)));
        rig.host
            .change(|c| c.row = json!({"stage": "proposal-review", "blocked": "github-issue"}));
        *rig.host.refusal.borrow_mut() = "the proposal waits for review".into();
        assert_eq!(
            rig.tick(t, false, None),
            format!(
                "skipped: autopilot proposal-review: the proposal waits for review; resumes when {ISSUE_RESTAMP}"
            )
        );
        rig.refusal_clear();
        rig.host.change(|c| c.approved_run = false);
        assert_eq!(
            rig.tick(t, false, None),
            format!(
                "skipped: autopilot continuation at stage proposal-review is not an approved `run`; resumes when {ISSUE_RESTAMP}"
            )
        );
        *rig.host.contract.borrow_mut() = Ok(contract("driving", OWNER, &"e".repeat(64)));
        *rig.host.refusal.borrow_mut() = "digest gate: bound base ref moved".into();
        assert!(rig.tick(t, false, None).starts_with("skipped: autopilot driving: digest gate: bound base ref moved; resumes when `oms autopilot --repo "));
        rig.refusal_clear();
        rig.host.change(|c| c.approved_run = false);
        assert!(rig.tick(t, false, None).starts_with(
            "skipped: autopilot continuation at stage driving is not an approved `run`"
        ));
        rig.host.change(|c| c.approved_run = true);
        // A live main (or unverified liveness) wins before any receipt is read.
        rig.fake.live_main();
        let live = rig.tick(t, false, None);
        assert!(live.starts_with("skipped: a panel main may be live (this panel already has a live main or unverified owner; if crashed: oms panel --release-main att_"), "{live}");
        assert!(
            rig.host.launched.borrow().is_empty() && !rig.dir.tick("last.json").exists(),
            "nothing declined wrote a ledger"
        );
    }
    impl Rig {
        fn refusal_clear(&self) {
            self.host.refusal.borrow_mut().clear();
        }
    }
    #[test]
    fn dry_runs_write_and_spawn_nothing() {
        let rig = Rig::new("driving");
        let dry = rig.env("2026-11-10T09:00:00Z").resume(true, None, false);
        assert!(
            dry.starts_with(&format!(
                "would resume autopilot (stage driving, receipt {}): oms autopilot",
                "d".repeat(12)
            )),
            "{dry}"
        );
        assert!(
            rig.host.launched.borrow().is_empty()
                && !rig.dir.tick("last.json").exists()
                && !rig.dir.tick("schedules.jsonl").exists()
        );
        *rig.host.stale.borrow_mut() = "autopilot approval is 9d old".into();
        assert!(
            rig.env("2026-11-10T09:00:00Z")
                .resume(true, None, false)
                .starts_with("skipped: autopilot approval is 9d old")
        );
        assert!(
            !rig.dir.tick("last.json").exists(),
            "a dry run records no stale skip"
        );
    }
    #[test]
    fn a_tick_receipt_that_cannot_be_trusted_refuses_and_is_never_rewritten() {
        let rig = Rig::new("driving");
        let t = "2026-11-10T09:00:00Z";
        let bad = [
            "",
            "{",
            "[]",
            "{\"wakes\":{}}",
            "{\"wakes\":[{\"day\":\"2026-11-10\"}]}",
            "{\"wakes\":[{\"day\":1,\"main\":\"m\"}]}",
            "{\"wakes\":[{\"day\":\"2026-11-09\",\"main\":\"m\"}]}",
            "{\"wakes\":[{\"day\":\"2026-11-10\",\"main\":\"m\"}]}",
            "{\"wakes\":[{\"day\":\"2026-11-10\",\"main\":\"m\",\"key\":\"\",\"outcome\":\"woke\"}]}",
            "{\"wakes\":[{\"day\":\"2026-11-10\",\"main\":\"m\",\"key\":\"k\",\"outcome\":\"bogus\"}]}",
        ];
        for body in bad {
            rig.dir.put(".oms/tick/last.json", body);
            let status = rig.tick(t, false, None);
            assert!(
                status.starts_with("refused: tick receipt") && status.contains("kept as found"),
                "{body:?}: {status}"
            );
            assert_eq!(rig.dir.read(".oms/tick/last.json"), body);
            let json = wake_json_at(
                false,
                &rig.dir.0,
                &rig.host,
                &rig.fake,
                &rig.stop,
                at(t),
                None,
            );
            let receipt: Value = serde_json::from_slice(&json.1).unwrap();
            assert_eq!((json.0, receipt["wake"].as_i64()), (0, Some(0)));
            assert!(
                receipt.get("wakes").is_none()
                    && receipt["status"].as_str().unwrap().starts_with("refused:"),
                "{receipt}"
            );
        }
        let parked = Rig::new("parked");
        let bad_prev = "{\"wakes\":[{\"day\":\"2026-11-09\",\"main\":\"m\"}]}";
        parked.dir.put(".oms/tick/last.json", bad_prev);
        let parked_json = wake_json_at(
            false,
            &parked.dir.0,
            &parked.host,
            &parked.fake,
            &parked.stop,
            at(t),
            None,
        );
        let parked_receipt: Value = serde_json::from_slice(&parked_json.1).unwrap();
        assert_eq!(
            (parked_json.0, parked_receipt["wake"].as_i64()),
            (0, Some(0))
        );
        assert!(
            parked_receipt.get("wakes").is_none()
                && parked_receipt["status"]
                    .as_str()
                    .unwrap()
                    .starts_with("refused:"),
            "{parked_receipt}"
        );
        assert_eq!(parked.dir.read(".oms/tick/last.json"), bad_prev);
        std::fs::remove_file(rig.dir.tick("last.json")).unwrap();
        rig.dir.put(".oms/tick/elsewhere.json", "{\"wakes\":[]}");
        std::os::unix::fs::symlink("elsewhere.json", rig.dir.tick("last.json")).unwrap();
        assert!(
            rig.tick(t, false, None)
                .starts_with("refused: tick receipt is a symlink, a hard link")
        );
        std::fs::remove_file(rig.dir.tick("last.json")).unwrap();
        std::fs::hard_link(rig.dir.tick("elsewhere.json"), rig.dir.tick("last.json")).unwrap();
        assert!(
            rig.tick(t, false, None)
                .starts_with("refused: tick receipt is a symlink, a hard link")
        );
        assert_eq!(rig.dir.read(".oms/tick/elsewhere.json"), "{\"wakes\":[]}");
        std::fs::remove_file(rig.dir.tick("last.json")).unwrap();
        rig.dir.put(
            ".oms/tick/last.json",
            &format!("{{\"pad\":\"{}\"}}", "x".repeat(TICK_RECEIPT_MAX)),
        );
        assert!(rig.tick(t, false, None).contains("too large"));
        assert!(rig.host.launched.borrow().is_empty());
    }
    #[test]
    fn the_wake_receipt_keeps_every_current_day_entry_and_reports_the_decision() {
        let rig = Rig::new("driving");
        let t = at("2026-11-10T09:00:00Z");
        let today = vec![
            wake_row("2026-11-10", "call-a", "k1", "woke"),
            wake_row("2026-11-10", "autopilot", "k2", "skipped"),
        ];
        rig.seed(json!(today));
        let out = wake_json_at(false, &rig.dir.0, &rig.host, &rig.fake, &rig.stop, t, None);
        let receipt: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(out.0, 0);
        assert_eq!(receipt["wake"], 1);
        assert!(
            receipt["status"]
                .as_str()
                .unwrap()
                .starts_with("resumed autopilot")
        );
        let wakes = receipt["wakes"].as_array().unwrap();
        assert!(
            today.iter().all(|row| wakes.contains(row)),
            "no current-day entry may vanish: {wakes:?}"
        );
        assert_eq!(wakes.len(), 3);
        assert_eq!(
            wakes.as_slice(),
            rig.dir.wakes().as_slice(),
            "the receipt reports the ledger exactly as the file holds it"
        );
        // The next call declines and still reports the whole ledger.
        let out = wake_json_at(false, &rig.dir.0, &rig.host, &rig.fake, &rig.stop, t, None);
        let receipt: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(
            (
                receipt["wake"].as_i64(),
                receipt["wakes"].as_array().map(Vec::len)
            ),
            (Some(0), Some(3))
        );
        // A missing receipt is an empty ledger, not a refusal.
        let fresh = Rig::new("driving");
        *fresh.host.contract.borrow_mut() = Err(ContractError::Missing);
        let out = wake_json_at(
            true,
            &fresh.dir.0,
            &fresh.host,
            &fresh.fake,
            &fresh.stop,
            t,
            None,
        );
        let receipt: Value = serde_json::from_slice(&out.1).unwrap();
        assert_eq!(
            (
                receipt["wake"].as_i64(),
                receipt["wakes"].as_array().map(Vec::len)
            ),
            (Some(0), Some(0))
        );
        // A standalone wake defers while a sweep holds `.oms/tick/last.json`'s lock, so
        // an in-progress sweep never overwrites a concurrent wake's quota or dedupe row.
        let concurrent = Rig::new("driving");
        concurrent.seed(json!(today));
        let mut settings = oms_lock::Settings::from_env().unwrap();
        settings.timeout = Duration::from_secs(2);
        let guard = oms_lock::Lock::for_file(&receipt_path(&concurrent.dir.0), settings)
            .unwrap()
            .acquire(None)
            .unwrap();
        assert_eq!(
            concurrent
                .env("2026-11-10T09:00:00Z")
                .resume(false, None, false),
            "skipped: tick maintenance lock held"
        );
        assert_eq!(concurrent.dir.wakes(), today);
        let in_sweep = wake_json_at(
            false,
            &concurrent.dir.0,
            &concurrent.host,
            &concurrent.fake,
            &concurrent.stop,
            t,
            None,
        );
        assert_eq!(in_sweep.0, 0);
        assert_eq!(concurrent.dir.wakes().len(), 3);
        drop(guard);
    }
    #[test]
    fn a_scheduled_occurrence_recovers_prelaunch_intent_and_never_replays_unknown_launches() {
        for ledger_missing in [false, true] {
            let retry = Rig::new("driving");
            retry.schedule(
                "2026-11-10T08:00:00Z",
                &["--contract", OWNER, "--every", "1"],
            );
            let mut bounded = retry.env("2026-11-10T09:00:00Z");
            bounded.schedule_max = retry.log().len() + 420;
            let refused = bounded.resume(false, None, true);
            assert!(
                retry.host.launched.borrow().is_empty(),
                "a pre-launch persistence failure must not spawn: {refused}"
            );
            assert!(refused.contains("would exceed"), "{refused}");
            if ledger_missing {
                std::fs::remove_file(retry.dir.tick("last.json")).unwrap();
            }
            assert!(
                retry
                    .tick("2026-11-10T09:05:00Z", true, None)
                    .starts_with("resumed autopilot")
            );
            assert_eq!(retry.host.launched.borrow().len(), 1);
            assert_eq!(
                retry.dir.wakes().len(),
                1,
                "retrying a prepared intent does not spend another wake"
            );
            assert!(
                retry
                    .tick("2026-11-10T09:06:00Z", true, None)
                    .contains("not due")
            );
        }

        let rig = Rig::new("driving");
        rig.schedule(
            "2026-11-09T21:10:00Z",
            &["--contract", OWNER, "--every", "2"],
        );
        let before = "2026-11-09T22:59:00Z";
        for scheduled in [true, false] {
            assert_eq!(
                rig.tick(before, scheduled, None),
                format!(
                    "skipped: schedule for {OWNER} not due; resumes when the next occurrence is due at 2026-11-09T23:00:00Z"
                ),
                "a schedule also governs the unscheduled wake"
            );
        }
        let due = "2026-11-10T04:30:00Z";
        let status = rig.tick(due, true, None);
        assert!(
            status.starts_with(
                "resumed autopilot (schedule occurrence 2026-11-10T03:00:00Z, stage driving"
            ),
            "missed occurrences coalesce: {status}"
        );
        let events: Vec<Value> = rig
            .log()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[1]["event"], "launch_intent");
        assert_eq!(events[2]["event"], "launching");
        assert_eq!(events[3]["event"], "fired");
        assert_eq!(events[3]["pid"], 4242);
        assert_eq!(events[1]["occurrence"], "2026-11-10T03:00:00Z");
        let wakes = rig.dir.wakes();
        assert_eq!(
            (wakes[0]["schedule"].as_str(), wakes[0]["key"].as_str()),
            (
                Some("2026-11-10T03:00:00Z"),
                Some(hash16(&["autopilot", OWNER, "2026-11-10T03:00:00Z"]).as_str())
            )
        );
        assert!(
            rig.tick(due, true, None).contains(
                "not due; resumes when the next occurrence is due at 2026-11-10T05:00:00Z"
            )
        );
        assert!(
            rig.schedule(due, &["--list"])
                .1
                .contains("last run 2026-11-10T04:30:00Z (occurrence 2026-11-10T03:00:00Z)")
        );
        assert_eq!(rig.host.launched.borrow().len(), 1);
        // A missing ledger after acknowledgment never replays the effect.
        rig.dir.put(".oms/tick/last.json", "{\"wakes\":[]}");
        assert!(rig.tick(due, true, None).contains("not due"));
        // A missing acknowledgment cannot distinguish a kill before or after
        // the host call. Both residues refuse automatic replay, even next day.
        let unknown = events[..3]
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>();
        rig.dir.put(".oms/tick/schedules.jsonl", &unknown);
        assert!(
            rig.tick(due, true, None)
                .contains("outcome unknown; reconcile needed")
        );
        assert!(
            rig.tick("2026-11-11T09:00:00Z", true, None)
                .contains("outcome unknown; reconcile needed")
        );
        assert_eq!(rig.host.launched.borrow().len(), 1);
        assert!(
            rig.schedule(due, &["--list"])
                .1
                .contains("automatic retry refused")
        );
        rig.host.launched.borrow_mut().clear();
        assert!(
            rig.tick(due, true, None)
                .contains("outcome unknown; reconcile needed")
        );
        assert!(rig.host.launched.borrow().is_empty());
        // Pre-protocol `fired` events stay consumed without inventing a safe
        // retry proof from a missing wake ledger.
        let mut legacy = events[3].clone();
        legacy.as_object_mut().unwrap().remove("launch_id");
        legacy.as_object_mut().unwrap().remove("pid");
        rig.dir.put(
            ".oms/tick/schedules.jsonl",
            &format!("{}\n{legacy}\n", events[0]),
        );
        assert!(rig.tick(due, true, None).contains("not due"));
    }
    #[test]
    fn a_scheduled_occurrence_declines_when_paused_expired_unbound_or_out_of_budget() {
        let rig = Rig::new("driving");
        let t0 = "2026-11-10T18:10:00Z";
        rig.schedule(
            t0,
            &[
                "--contract",
                OWNER,
                "--every",
                "1",
                "--until",
                "2026-11-10T21:00:00Z",
            ],
        );
        rig.schedule(t0, &["--pause", "--contract", OWNER]);
        assert_eq!(
            rig.tick("2026-11-10T19:05:00Z", true, None),
            format!(
                "skipped: schedule for {OWNER} paused; resumes when `oms tick schedule --resume --contract {OWNER}`"
            )
        );
        rig.schedule(t0, &["--resume", "--contract", OWNER]);
        // Budget exhausted: the occurrence is consumed, never queued.
        rig.seed(json!(
            (0..WAKE_DAILY_LIMIT)
                .map(|i| wake_row("2026-11-10", "call-other", &format!("k{i}"), "woke"))
                .collect::<Vec<_>>()
        ));
        let spent = rig.tick("2026-11-10T19:05:00Z", true, None);
        assert_eq!(
            spent,
            format!(
                "skipped: schedule for {OWNER} occurrence 2026-11-10T19:00:00Z skipped: 6 wakes already spent today; resumes when the next occurrence at 2026-11-10T20:00:00Z finds budget"
            )
        );
        let last: Value = serde_json::from_str(rig.log().lines().last().unwrap()).unwrap();
        assert_eq!(
            (
                last["event"].as_str(),
                last["reason"].as_str(),
                last["occurrence"].as_str()
            ),
            (
                Some("skipped"),
                Some("budget"),
                Some("2026-11-10T19:00:00Z")
            )
        );
        assert!(
            rig.tick("2026-11-10T19:06:00Z", true, None)
                .contains("not due"),
            "the consumed occurrence does not run later"
        );
        assert!(rig.host.launched.borrow().is_empty());
        rig.seed(json!([]));
        assert!(
            rig.tick("2026-11-10T20:30:00Z", true, None)
                .starts_with("resumed autopilot (schedule occurrence 2026-11-10T20:00:00Z")
        );
        assert_eq!(
            rig.tick("2026-11-10T21:00:00Z", true, None),
            format!(
                "skipped: schedule for {OWNER} expired at 2026-11-10T21:00:00Z; resumes when `oms tick schedule --cancel` or a new schedule after it"
            )
        );
        // Another contract's receipt is not bound by this schedule; unscheduled wakes keep their own rules.
        rig.host.change(|c| c.owner_id = OTHER.into());
        assert_eq!(
            rig.tick("2026-11-11T09:00:00Z", true, None),
            format!(
                "skipped: no schedule binds contract {OTHER}; resumes when `oms tick schedule --contract ID --every HOURS` binds one"
            )
        );
        assert!(
            rig.tick("2026-11-11T09:00:00Z", false, None)
                .starts_with("resumed autopilot (stage driving")
        );
        // The marker spends an exhausted run's occurrence too.
        let rig = Rig::new("driving");
        rig.schedule(t0, &["--contract", OWNER, "--every", "1"]);
        let marker = rig.dir.0.join("marker");
        assert!(
            rig.tick("2026-11-10T19:05:00Z", true, Some(&marker))
                .starts_with("resumed")
        );
        let second = rig.tick("2026-11-10T20:05:00Z", true, Some(&marker));
        assert!(second.contains("occurrence 2026-11-10T20:00:00Z skipped: this tick run already resumed one autopilot"), "{second}");
    }
    #[test]
    fn a_failed_spawn_leaves_the_recorded_run_in_place() {
        let rig = Rig::new("driving");
        rig.host.fail_launch.set(true);
        let status = rig.tick("2026-11-10T09:00:00Z", false, None);
        assert!(
            status.starts_with(
                "skipped: autopilot launch failed after it was recorded (exec refused)"
            ),
            "{status}"
        );
        assert_eq!(
            rig.dir.wakes().len(),
            1,
            "recorded before the spawn, so a retry never doubles it"
        );
        assert!(
            rig.tick("2026-11-10T09:30:00Z", false, None)
                .contains("outcome unknown; no resend")
        );
        let scheduled = Rig::new("driving");
        scheduled.schedule(
            "2026-11-10T08:00:00Z",
            &["--contract", OWNER, "--every", "1"],
        );
        scheduled.host.fail_launch.set(true);
        assert!(
            scheduled
                .tick("2026-11-10T09:00:00Z", true, None)
                .contains("outcome unknown; reconcile needed")
        );
        assert!(scheduled.log().contains("launch_unknown"));
        scheduled.host.fail_launch.set(false);
        assert!(
            scheduled
                .tick("2026-11-10T11:00:00Z", true, None)
                .contains("outcome unknown; reconcile needed")
        );
        assert!(
            scheduled.host.launched.borrow().is_empty(),
            "a generic host error does not prove no side effect"
        );
    }
    #[test]
    fn the_default_launch_detaches_with_the_wake_marker_and_a_private_log() {
        struct Shell;
        impl AutopilotHost for Shell {
            fn load_contract(
                &self,
                _: &Path,
            ) -> std::result::Result<ApprovedContract, ContractError> {
                Err(ContractError::Missing)
            }
            fn reentry_refusal(
                &self,
                _: &Path,
                _: &ApprovedContract,
            ) -> std::result::Result<String, String> {
                Ok(String::new())
            }
            fn stale_approval(&self, _: &Path, _: &ApprovedContract, _: i64) -> String {
                String::new()
            }
            fn executable(&self) -> std::io::Result<PathBuf> {
                Ok(PathBuf::from("/bin/sh"))
            }
        }
        let dir = Dir::new();
        let log = dir.0.join("run.log");
        let argv: Vec<String> = ["oms", "-c", "printf '%s' \"$OMS_AUTOPILOT_WAKE_RESUME\""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pid = Shell.launch(&argv, &log, &dir.0).unwrap();
        assert!(pid > 0);
        let started = std::time::Instant::now();
        while dir.read("run.log").is_empty() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(dir.read("run.log"), "1");
        assert_eq!(std::fs::metadata(&log).unwrap().mode() & 0o777, 0o600);
        std::os::unix::fs::symlink("run.log", dir.0.join("link.log")).unwrap();
        assert!(
            Shell
                .launch(&argv, &dir.0.join("link.log"), &dir.0)
                .is_err(),
            "a symlinked log is refused"
        );
        std::fs::hard_link(&log, dir.0.join("hard.log")).unwrap();
        assert!(
            Shell
                .launch(&argv, &dir.0.join("hard.log"), &dir.0)
                .is_err(),
            "a hard-linked log is refused"
        );
        assert_eq!(dir.read("run.log"), "1");
    }

    // ---- the typed wake

    fn typed_rig() -> Rig {
        let rig = Rig::new("driving");
        rig.host.tmux.set(true);
        rig.fake.live_main();
        rig.idle_since("2026-11-10T09:00:00Z", 3600);
        rig
    }
    const T: &str = "2026-11-10T09:00:00Z";
    fn wake(rig: &Rig, dry: bool) -> String {
        rig.env(T).wake_main(dry, None)
    }
    fn native_return_fixture(rig: &Rig) -> (Value, Value, String) {
        native_return_fixture_for(rig, ATT, ATT, "claude")
    }
    fn native_return_fixture_for(
        rig: &Rig,
        parent: &str,
        participant: &str,
        owner: &str,
    ) -> (Value, Value, String) {
        use crate::native_contract::{CheckedAssignment, execution_contract};
        let attempt = "att_cccccccccccccccccccccccccccccccc";
        let task = json!({"id":"native-task","title":"Inspect the source","role":"researcher",
            "plan_id":format!("plan_{}","a".repeat(32)),"lease_id":format!("lease_{}","b".repeat(32)),
            "lease_epoch":1,"provider":owner,"claimed_by_participant":participant,"state":"claimed",
            "claim_expired":false,"contract_stale":false,"allowed_paths":["src"],
            "forbidden_paths":[],"verify":"cargo test","covers":["criterion-one"]});
        let mut packet = json!({"schema":1,"kind":"oms-native-assignment","parent_attempt_id":parent,"parent_participant":participant,
            "provider":"codex","room":ROOM,"round":1,"session":"oms-fixture",
            "main_pane":{"id":PANE,"window":"@2","pid":"4242","birth":"main-birth"},
            "lead_pane":{"id":"%9","window":"@3","pid":"4243","birth":"lead-birth"},
            "account":null,"account_verification":"unknown",
            "checkout":{"cwd":rig.dir.0,"worktree":rig.dir.0,"base_sha":"c".repeat(40),"write_isolation":"unmanaged"},
            "result_contract":execution_contract(&task).unwrap(),"task":task});
        let intent = crate::digest(&serde_json::to_vec(&packet).unwrap());
        packet["attempt_id"] = attempt.into();
        let raw = serde_json::to_vec(&packet).unwrap();
        let assignment = CheckedAssignment::parse(&raw).unwrap().unwrap();
        let binding = format!(".oms/lifecycle/attempts/{attempt}/native-assignment.json");
        rig.dir.put(&binding, std::str::from_utf8(&raw).unwrap());
        let paths = crate::native_results::input_paths(&assignment);
        let mut source = assignment.report_template();
        source["summary"] = "Source observations await Main review".into();
        let source = serde_json::to_string(&source).unwrap();
        rig.dir.put(paths["source"].as_str().unwrap(), &source);
        let manifest = json!({"schema":1,"attempt_id":attempt,"binding_sha256":assignment.sha256(),
            "previous_sha256":"","kind":"findings","summary":"Source observations await Main review",
            "source":{"path":paths["source"],"sha256":crate::digest(source.as_bytes())},
            "patch":null,"verification":[]});
        let path = paths["manifest"].as_str().unwrap().to_owned();
        rig.dir.put(&path, &manifest.to_string());
        let row = json!({"attempt_id":attempt,"tool":"panel-native-assignment","provider":"codex",
            "parent_attempt_id":parent,"task_id":"native-task","state":"working","terminal":false,
            "refs":{"panel_room_id":ROOM,"panel_owner":owner,"plan_id":task["plan_id"],
                "lease_id":task["lease_id"],"native_assignment_schema":"1","native_return_schema":"1",
                "native_transport":"submitted","native_intent_sha256":intent,"native_binding":binding,
                "native_binding_sha256":assignment.sha256(),"native_round":"1","native_resolution":"awaiting-report"}});
        (row, packet, path)
    }
    #[test]
    fn native_return_wakes_follow_checked_main_succession_without_rebinding_work() {
        let rig = typed_rig();
        rig.fake.ready.borrow_mut().clear();
        let old = "att_dddddddddddddddddddddddddddddddd";
        let mut outgoing = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let exited = crate::terminals::process(outgoing.id()).unwrap();
        let live = crate::terminals::process(std::process::id()).unwrap();
        rig.fake.main_pid.set(live.pid);
        {
            let mut history = rig.fake.history.borrow_mut();
            let refs = &mut history[0]["refs"];
            refs["panel_owner"] = "claude".into();
            refs["panel_model"] = "fixture-successor".into();
            refs["panel_native_scheduling"] = "unmanaged".into();
            history.push(json!({"attempt_id":old,"tool":"panel-main","provider":"claude",
                "terminal":true,"state":"abandoned","reason_code":"main_swapped",
                "refs":{"panel_room_id":ROOM,"panel_room_participant":MEMBER,
                    "panel_native_scheduling":"unmanaged","panel_wrapper_pid":exited.pid.to_string(),
                    "panel_wrapper_identity":exited.birth,"panel_native_pid":exited.pid.to_string(),
                    "panel_native_identity":exited.birth}}));
        }
        rig.fake
            .room_override
            .replace(Some(json!({"id":ROOM,"closed":false,"successor":null,
            "participants":[{"participant":MEMBER,"joined":true,"role":"main","provider":"claude"}],
            "swaps":[{"participant":MEMBER,"attempt":old,"from":"claude","to":"claude",
                "harness":"claude","model":"fixture-successor"}]})));
        let (mut row, packet, path) = native_return_fixture_for(&rig, old, MEMBER, "claude");
        let assignment =
            crate::native_contract::CheckedAssignment::parse(&serde_json::to_vec(&packet).unwrap())
                .unwrap()
                .unwrap();
        let mut record = json!({"schema":1,"kind":"oms-native-main-succession","mode":"continuation",
            "attempt_id":assignment.attempt_id(),"parent_attempt_id":old,"room":ROOM,
            "task_sha256":assignment.task_sha256(),"checkout_sha256":assignment.checkout_sha256(),
            "assignment":{"path":row["refs"]["native_binding"],"sha256":assignment.sha256(),"round":1},
            "result_sha256":null,"previous":null,
            "custody":{"main_attempt_id":ATT,"main_owner":"claude","participant":MEMBER,
                "session":{"session":"$1","server_pid":live.pid,"server_birth":live.birth,
                    "socket_digest":crate::digest(b"fixture-socket")},
                "main_pane":{"id":PANE,"window":"@2","pid":live.pid,"birth":live.birth},
                "lead_pane":packet["lead_pane"],"account":packet["account"],
                "account_verification":packet["account_verification"],"checkout":packet["checkout"]}});
        let write_record = |row: &mut Value, record: &Value| {
            let bytes = serde_json::to_vec(record).unwrap();
            let digest = crate::digest(&bytes);
            let path = format!(
                ".oms/lifecycle/attempts/{}/native-succession-{digest}.json",
                assignment.attempt_id()
            );
            rig.dir.put(&path, std::str::from_utf8(&bytes).unwrap());
            row["refs"]["native_succession_schema"] = "1".into();
            row["refs"]["native_succession_path"] = path.into();
            row["refs"]["native_succession_sha256"] = digest.into();
            row["refs"]["native_custodian_attempt"] = ATT.into();
        };
        // A cue delivered to the outgoing Main does not consume the successor's
        // cue for the same still-unimported bytes, but both spend the same quota.
        let successor_history = rig.fake.history.borrow().clone();
        let successor_room = rig.fake.room_override.borrow().clone();
        let mut outgoing_row = successor_history[0].clone();
        outgoing_row["attempt_id"] = old.into();
        rig.fake.history.replace(vec![outgoing_row]);
        let mut old_room = successor_room.clone().unwrap();
        old_room["swaps"] = json!([]);
        rig.fake.room_override.replace(Some(old_room));
        rig.fake.main_attempt.replace(old.into());
        rig.fake.main_pid.set(exited.pid);
        rig.idle_since(T, 3600);
        rig.fake.children.borrow_mut().push(row.clone());
        let old_cue = wake(&rig, false);
        assert!(
            old_cue.starts_with("woke ") && old_cue.contains("native-task (import)"),
            "{old_cue}"
        );
        assert!(wake(&rig, false).contains("already woken today"));
        let old_key = rig.dir.wakes()[0]["key"].clone();
        outgoing.kill().unwrap();
        outgoing.wait().unwrap();
        rig.fake.history.replace(successor_history);
        rig.fake.room_override.replace(successor_room);
        rig.fake.main_attempt.replace(ATT.into());
        rig.fake.main_pid.set(live.pid);
        rig.idle_since(T, 3600);
        write_record(&mut row, &record);
        rig.fake.children.borrow_mut()[0] = row.clone();
        let first = wake(&rig, false);
        assert!(
            first.starts_with("woke ") && first.contains("native-task (import)"),
            "{first}"
        );
        assert_ne!(rig.dir.wakes()[1]["key"], old_key);
        assert!(wake(&rig, false).contains("already woken today"));
        let store = Store::open(&rig.dir.0).unwrap();
        assert!(crate::native_results::read(&store, &row).unwrap().is_none());
        let capture = crate::native_results::CaptureInput::read(&rig.dir.0, Path::new(&path))
            .unwrap()
            .capture(&rig.dir.0, Some(&assignment))
            .unwrap();
        let retained = {
            let tx = store
                .transaction("native-succession-fixture", Duration::from_secs(1))
                .unwrap();
            capture
                .retain(
                    &tx,
                    &packet,
                    row["refs"]["native_binding"].as_str().unwrap(),
                )
                .unwrap()
        };
        row["refs"]["native_result_schema"] = "1".into();
        row["refs"]["native_result_path"] = retained["path"].clone();
        row["refs"]["native_result_sha256"] = retained["sha256"].clone();
        row["refs"]["native_resolution"] = "pending".into();
        rig.fake.children.borrow_mut()[0] = row.clone();
        let result = wake(&rig, false);
        assert!(
            result.starts_with("woke ") && result.contains("native-task (reconciliation)"),
            "{result}"
        );
        assert!(wake(&rig, false).contains("already woken today"));
        row["refs"]["native_resolution"] = "request-revision".into();
        row["refs"]["native_resolution_result_sha256"] = retained["sha256"].clone();
        rig.fake.children.borrow_mut()[0] = row.clone();
        assert!(wake(&rig, true).contains("native-task (continuation)"));
        // A retained record is not live authority. Refuse changed process,
        // model, membership, chain or partial refs without sending another cue.
        let sends = rig.fake.sent.borrow().len();
        rig.fake.main_pid.set(exited.pid);
        assert!(!wake(&rig, false).starts_with("woke "));
        rig.fake.main_pid.set(live.pid);
        rig.fake.history.borrow_mut()[0]["refs"]["panel_model"] = "other".into();
        assert!(wake(&rig, false).contains("canonical swap target"));
        rig.fake.history.borrow_mut()[0]["refs"]["panel_model"] = "fixture-successor".into();
        rig.fake.children.borrow_mut()[0]["refs"]
            .as_object_mut()
            .unwrap()
            .remove("native_succession_sha256");
        assert!(wake(&rig, false).contains("native succession evidence unavailable"));
        rig.fake.children.borrow_mut()[0] = row.clone();
        let active_room = rig.fake.room_override.borrow().clone().unwrap();
        for invalid in [
            json!({"successor":"room-fedcba654321"}),
            json!({"closed":true}),
            json!({"participants":[{"participant":MEMBER,"joined":true,
                "role":"main","provider":"claude","parent":old}]}),
        ] {
            let mut invalid_room = active_room.clone();
            invalid_room
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            rig.fake.room_override.replace(Some(invalid_room));
            assert!(wake(&rig, false).contains("exact current Room Main"));
        }
        let mut incomplete = active_room.clone();
        incomplete.as_object_mut().unwrap().remove("successor");
        rig.fake.room_override.replace(Some(incomplete));
        assert!(wake(&rig, false).contains("exact current Room Main"));
        rig.fake.room_override.replace(Some(active_room));
        assert_eq!(rig.fake.sent.borrow().len(), sends);
        // A settled-only adoption only asks Main to retire the assignment. The
        // old input is never reopened, and its historical parent/claim stay fixed.
        record["mode"] = "retirement".into();
        record["result_sha256"] = retained["sha256"].clone();
        write_record(&mut row, &record);
        row["terminal"] = true.into();
        row["refs"]["native_resolution"] = "retiring".into();
        rig.fake.children.borrow_mut()[0] = row.clone();
        let retiring = wake(&rig, false);
        assert!(
            retiring.starts_with("woke ") && retiring.contains("native-task (retirement)"),
            "{retiring}"
        );
        assert!(wake(&rig, false).contains("already woken today"));
        rig.fake.children.borrow_mut()[0]["refs"]["native_resolution"] = "retired".into();
        assert!(wake(&rig, true).contains("no ready plan work"));
        assert_eq!(row["parent_attempt_id"], old);
        assert_eq!(assignment.parent_participant(), MEMBER);
        assert_eq!(
            rig.dir.wakes().len(),
            4,
            "one existing wake budget covers all successors and phases"
        );
    }
    #[test]
    fn native_return_wakes_bind_the_current_owner_and_exact_import_or_result() {
        let rig = typed_rig();
        rig.fake.ready.borrow_mut().clear();
        let (row, packet, path) = native_return_fixture(&rig);
        rig.fake.children.borrow_mut().push(row.clone());
        let first = wake(&rig, false);
        assert!(
            first.starts_with("woke ") && first.contains("native-task (import)"),
            "{first}"
        );
        let store = Store::open(&rig.dir.0).unwrap();
        assert!(
            crate::native_results::read(&store, &row).unwrap().is_none(),
            "wake never imports"
        );
        assert!(wake(&rig, false).contains("already woken today"));
        let first_key = rig.dir.wakes()[0]["key"].clone();
        let original = rig.dir.read(&path);
        let mut changed: Value = serde_json::from_str(&original).unwrap();
        changed["summary"] = "Updated untrusted observations".into();
        rig.dir.put(&path, &changed.to_string());
        assert!(
            wake(&rig, false).starts_with("woke "),
            "new input bytes are distinct work"
        );
        assert_ne!(rig.dir.wakes()[1]["key"], first_key);
        rig.dir.put(&path, &original);

        let assignment =
            crate::native_contract::CheckedAssignment::parse(&serde_json::to_vec(&packet).unwrap())
                .unwrap()
                .unwrap();
        let capture = crate::native_results::CaptureInput::read(&rig.dir.0, Path::new(&path))
            .unwrap()
            .capture(&rig.dir.0, Some(&assignment))
            .unwrap();
        let retained = {
            let tx = store
                .transaction("native-return-fixture", Duration::from_secs(1))
                .unwrap();
            capture
                .retain(
                    &tx,
                    &packet,
                    row["refs"]["native_binding"].as_str().unwrap(),
                )
                .unwrap()
        };
        {
            let mut rows = rig.fake.children.borrow_mut();
            let refs = &mut rows[0]["refs"];
            refs["native_result_schema"] = "1".into();
            refs["native_result_path"] = retained["path"].clone();
            refs["native_result_sha256"] = retained["sha256"].clone();
            refs["native_resolution"] = "pending".into();
            refs["native_resolution_result_sha256"] = "".into();
        }
        let reported = wake(&rig, false);
        assert!(
            reported.starts_with("woke ") && reported.contains("native-task (reconciliation)"),
            "{reported}"
        );
        assert!(wake(&rig, false).contains("already woken today"));
        for decision in ["reviewed", "blocked", "request-revision"] {
            {
                let mut rows = rig.fake.children.borrow_mut();
                rows[0]["refs"]["native_resolution"] = decision.into();
                rows[0]["refs"]["native_resolution_result_sha256"] = retained["sha256"].clone();
            }
            let status = wake(&rig, true);
            if decision == "request-revision" {
                assert!(
                    status.starts_with("would wake ")
                        && status.contains("native-task (continuation)"),
                    "{status}"
                );
            } else {
                assert!(status.contains("no ready plan work"), "{status}");
            }
        }
        rig.fake.children.borrow_mut()[0]["refs"]["native_resolution"] = "reviewed".into();
        changed["previous_sha256"] = retained["sha256"].clone();
        rig.dir.put(&path, &changed.to_string());
        assert!(wake(&rig, false).contains("reviewed native return has changed input"));
        rig.dir.put(&path, &original);
        rig.fake.children.borrow_mut()[0]["refs"]["native_resolution_result_sha256"] =
            "d".repeat(64).into();
        assert!(wake(&rig, false).contains("resolution differs"));
        assert_eq!(
            rig.dir.wakes().len(),
            3,
            "refusal and dry runs spend no wake"
        );
    }
    #[test]
    fn native_return_wakes_refuse_unsafe_inputs_and_keep_existing_wake_guards() {
        use std::os::unix::fs::symlink;
        let rig = typed_rig();
        rig.fake.ready.borrow_mut().clear();
        let (row, _, path) = native_return_fixture(&rig);
        rig.fake.children.borrow_mut().push(row.clone());
        for (field, value) in [
            ("parent_attempt_id", json!("other-main")),
            ("terminal", json!(true)),
        ] {
            rig.fake.children.borrow_mut()[0][field] = value;
            assert!(wake(&rig, true).contains("no ready plan work"));
            rig.fake.children.borrow_mut()[0] = row.clone();
        }
        for field in ["panel_owner", "panel_room_id"] {
            rig.fake.children.borrow_mut()[0]["refs"][field] = "other".into();
            assert!(wake(&rig, true).contains("no ready plan work"));
            rig.fake.children.borrow_mut()[0] = row.clone();
        }
        let original = rig.dir.read(&path);
        let original_input: Value = serde_json::from_str(&original).unwrap();
        let source_path = original_input["source"]["path"].as_str().unwrap();
        let source = rig.dir.read(source_path);
        for field in ["attempt_id", "binding_sha256", "previous_sha256"] {
            let mut input: Value = serde_json::from_str(&original).unwrap();
            input[field] = if field == "attempt_id" {
                "other-attempt".into()
            } else {
                "d".repeat(64).into()
            };
            rig.dir.put(&path, &input.to_string());
            let status = wake(&rig, false);
            assert!(
                status.contains("native return input differs")
                    || status.contains("input predecessor differs"),
                "{status}"
            );
        }
        rig.dir.put(&path, "{}");
        assert!(wake(&rig, false).contains("input unavailable"));
        rig.dir.put(&path, &"x".repeat(64 * 1024 + 1));
        assert!(wake(&rig, false).contains("input presence is unknown"));
        std::fs::remove_file(rig.dir.0.join(&path)).unwrap();
        assert!(wake(&rig, true).contains("no ready plan work"));
        rig.dir.put("untrusted-input.json", &original);
        symlink(
            rig.dir.0.join("untrusted-input.json"),
            rig.dir.0.join(&path),
        )
        .unwrap();
        assert!(wake(&rig, false).contains("input presence is unknown"));
        std::fs::remove_file(rig.dir.0.join(&path)).unwrap();
        rig.dir.put(&path, &original);
        rig.dir.put(source_path, &"x".repeat(64 * 1024 + 1));
        assert!(wake(&rig, false).contains("source is absent or unsafe"));
        std::fs::remove_file(rig.dir.0.join(source_path)).unwrap();
        symlink(
            rig.dir.0.join("untrusted-input.json"),
            rig.dir.0.join(source_path),
        )
        .unwrap();
        assert!(wake(&rig, false).contains("source is absent or unsafe"));
        std::fs::remove_file(rig.dir.0.join(source_path)).unwrap();
        rig.dir.put(source_path, &source);
        rig.fake.children.borrow_mut()[0]["refs"]["native_binding"] = ".oms/unrelated.json".into();
        assert!(wake(&rig, false).contains("metadata unavailable"));
        rig.fake.children.borrow_mut()[0] = row.clone();
        rig.idle_since(T, 1799);
        assert_eq!(
            wake(&rig, false),
            "skipped: main is not idle for 30 minutes"
        );
        rig.idle_since(T, 1800);
        rig.fake.children.borrow_mut().push(json!({"attempt_id":"child","terminal":false,"refs":{"panel_room_id":ROOM,"panel_role":"worker"}}));
        assert_eq!(wake(&rig, false), "skipped: main has a live child call");
        rig.fake.children.borrow_mut().pop();
        rig.seed(json!(
            (0..WAKE_DAILY_LIMIT)
                .map(|n| wake_row("2026-11-10", "other", &format!("k{n}"), "woke"))
                .collect::<Vec<_>>()
        ));
        assert!(wake(&rig, false).contains("reached 6 wakes today"));
        assert!(rig.fake.sent.borrow().is_empty());
        rig.fake.children.borrow_mut().resize(21, row);
        assert!(wake(&rig, false).contains("exceeds 20 active assignments"));

        let raced = typed_rig();
        raced.fake.ready.borrow_mut().clear();
        let (row, _, path) = native_return_fixture(&raced);
        raced.fake.children.borrow_mut().push(row);
        let changed = || {
            let mut input: Value = serde_json::from_str(&raced.dir.read(&path)).unwrap();
            input["summary"] = "changed before wake lock".into();
            raced.dir.put(&path, &input.to_string());
        };
        let mut env = raced.env(T);
        env.after_panel_lock = Some(&changed);
        assert!(
            env.wake_main(false, None)
                .contains("shared work changed before wake")
        );
        assert!(raced.fake.sent.borrow().is_empty());
        assert!(!raced.dir.0.join(".oms/tick/last.json").exists());
    }
    #[test]
    fn external_pending_work_never_polls_a_model_and_terminal_wakes_once_across_days() {
        let rig = typed_rig();
        let binding = "b".repeat(64);
        let result = "c".repeat(64);
        let mut receipt = json!({"stage":"parked","owner_id":OWNER,"adaptive":{"wait":{"binding_sha256":binding,"consumed_result_sha256":""}}});
        rig.dir
            .put(".oms/plan/autopilot-run.json", &receipt.to_string());
        for state in ["pending", "running", "unknown"] {
            *rig.host.external.borrow_mut() = json!({"schema":1,"owner_id":OWNER,"binding_sha256":binding,"identity_valid":true,"status":state,"result_sha256":null,"action":"inspect"});
            assert!(wake(&rig, false).contains("no ready plan work"));
            assert!(
                rig.fake.sent.borrow().is_empty(),
                "ready tasks must not cause polling during {state}"
            );
        }
        *rig.host.external.borrow_mut() = json!({"schema":1,"owner_id":OWNER,"binding_sha256":binding,"identity_valid":true,"status":"terminal","result_sha256":result,"action":"resume"});
        assert!(wake(&rig, false).starts_with("woke "));
        assert_eq!(rig.fake.sent.borrow().len(), 2);
        assert!(wake(&rig, false).contains("already woken for this exact external result"));
        assert!(
            rig.env("2026-11-11T09:00:00Z")
                .wake_main(false, None)
                .contains("already woken for this exact external result")
        );
        assert_eq!(rig.fake.sent.borrow().len(), 2);
        receipt["adaptive"]["wait"]["consumed_result_sha256"] = result.into();
        rig.dir
            .put(".oms/plan/autopilot-run.json", &receipt.to_string());
        assert!(wake(&rig, false).contains("no ready plan work"));
        rig.host.external.borrow_mut()["owner_id"] = OTHER.into();
        assert!(wake(&rig, false).contains("custody changed"));
        assert_eq!(rig.fake.sent.borrow().len(), 2);
    }

    #[test]
    fn a_new_verified_main_gets_one_unconsumed_result_cue() {
        let rig = typed_rig();
        let mut receipt = json!({"stage":"parked","owner_id":OWNER,"adaptive":{"wait":{"binding_sha256":"b".repeat(64),"consumed_result_sha256":""}}});
        rig.dir
            .put(".oms/plan/autopilot-run.json", &receipt.to_string());
        *rig.host.external.borrow_mut() = json!({"schema":1,"owner_id":OWNER,"binding_sha256":"b".repeat(64),"identity_valid":true,"status":"terminal","result_sha256":"c".repeat(64),"action":"resume"});
        assert!(wake(&rig, false).starts_with("woke "));
        assert!(wake(&rig, false).contains("already woken"));
        // The native Main-swap owner proves this transition; the tick fixture
        // exposes its new lifecycle/window/activity generation on the same
        // stable Room participant. A stale generation must still refuse.
        rig.fake.history.borrow_mut()[0]["terminal"] = true.into();
        *rig.fake.main_attempt.borrow_mut() = format!("att_{}", "c".repeat(32));
        rig.fake.live_main();
        assert!(wake(&rig, false).contains("not idle"));
        rig.dir.put(&format!(".oms/hooks/panel-activity/{MEMBER}.json"),&json!({"state":"idle","attempt":rig.fake.main_attempt.borrow().as_str(),"since":parse_utc(T).unwrap()-3600}).to_string());
        assert!(wake(&rig, false).starts_with("woke "));
        assert!(wake(&rig, false).contains("already woken"));
        assert_eq!(rig.fake.sent.borrow().len(), 4);
        receipt["adaptive"]["wait"]["consumed_result_sha256"] = "c".repeat(64).into();
        rig.dir
            .put(".oms/plan/autopilot-run.json", &receipt.to_string());
        assert!(wake(&rig, false).contains("no ready plan work"));
        assert_eq!(rig.fake.sent.borrow().len(), 4);
    }

    #[test]
    fn parent_stopped_contract_suppresses_pending_terminal_and_generic_work() {
        let rig = typed_rig();
        let mut receipt = json!({"stage":"parked","owner_id":OWNER,"diagnostic":{"reason_code":"parent-stopped"},"adaptive":{"wait":{"binding_sha256":"b".repeat(64),"consumed_result_sha256":""}}});
        for status in ["pending", "terminal"] {
            *rig.host.external.borrow_mut() = json!({"schema":1,"owner_id":OWNER,"binding_sha256":"b".repeat(64),"identity_valid":true,"status":status,"result_sha256":"c".repeat(64),"action":"resume"});
            rig.dir
                .put(".oms/plan/autopilot-run.json", &receipt.to_string());
            assert!(wake(&rig, false).contains("no ready plan work"));
            assert!(rig.fake.sent.borrow().is_empty());
        }
        receipt["adaptive"]["wait"] = Value::Null;
        rig.dir
            .put(".oms/plan/autopilot-run.json", &receipt.to_string());
        assert!(wake(&rig, false).contains("no ready plan work"));
        assert!(
            rig.fake.sent.borrow().is_empty(),
            "unfinished Plan work cannot undo Parent stop"
        );
        assert_eq!(
            rig.host.refreshes.get(),
            0,
            "stopped owner does not refresh an external wait"
        );
    }

    #[test]
    fn lost_launch_ack_requires_exact_start_finish_even_after_receipt_progress_and_restart() {
        let rig = Rig::new("driving");
        rig.host.fail_launch.set(true);
        assert!(
            rig.tick(T, false, None)
                .contains("launch failed after it was recorded")
        );
        let last: Value = serde_json::from_str(&rig.dir.read(".oms/tick/last.json")).unwrap();
        let pending = &last["pending_launch"];
        rig.host.fail_launch.set(false);
        rig.host.change(|c| {
            c.receipt_sha256 = "f".repeat(64);
            c.stage = "reviewing".into();
        });
        assert!(
            rig.tick("2026-11-11T09:00:00Z", false, None)
                .contains("outcome unknown; no resend")
        );
        assert!(rig.host.launched.borrow().is_empty());
        let mut start = json!({"schema":1,"kind":"autopilot-wake-launch","launch_id":pending["launch_id"],"owner_id":OWNER,"input_receipt_sha256":pending["receipt"],"phase":"started"});
        start["input_receipt_sha256"] = "a".repeat(64).into();
        rig.dir
            .put(".oms/plan/progress.jsonl", &format!("{start}\n"));
        assert!(
            rig.tick("2026-11-11T09:01:00Z", false, None)
                .contains("identity mismatch")
        );
        start["input_receipt_sha256"] = pending["receipt"].clone();
        rig.dir
            .put(".oms/plan/progress.jsonl", &format!("{start}\n"));
        assert!(
            rig.tick("2026-11-11T09:02:00Z", false, None)
                .contains("started without an exact finish")
        );
        let mut finish = start.clone();
        finish["phase"] = "finished".into();
        finish["exit"] = 3.into();
        rig.dir
            .put(".oms/plan/progress.jsonl", &format!("{start}\n{finish}\n"));
        assert!(
            rig.tick("2026-11-11T09:03:00Z", false, None)
                .contains("reconciled autopilot launch")
        );
        assert!(
            rig.host.launched.borrow().is_empty(),
            "reconciliation cannot resend the old command"
        );
        let after: Value = serde_json::from_str(&rig.dir.read(".oms/tick/last.json")).unwrap();
        assert!(after["pending_launch"].is_null());
    }
    #[test]
    fn an_idle_main_with_an_empty_prompt_is_woken_once_with_the_ledger_written_first() {
        let rig = typed_rig();
        let status = wake(&rig, false);
        let prompt = "OMS tick wake: ready plan tasks t1 ready, t2 ready. Continue the shared plan (oms agent-plan list) within your existing authority; if the next step needs the user's decision, authority you do not have, or a hard blocker, say which and stop.";
        assert_eq!(status, format!("woke {MEMBER}: {prompt}"));
        assert_eq!(
            *rig.fake.sent.borrow(),
            vec![
                vec![
                    "tmux".to_owned(),
                    "send-keys".into(),
                    "-t".into(),
                    PANE.into(),
                    "-l".into(),
                    prompt.into()
                ],
                vec![
                    "tmux".to_owned(),
                    "send-keys".into(),
                    "-t".into(),
                    PANE.into(),
                    "Enter".into()
                ]
            ]
        );
        let wakes = rig.dir.wakes();
        assert_eq!(
            (
                wakes.len(),
                wakes[0]["outcome"].as_str(),
                wakes[0]["main"].as_str()
            ),
            (1, Some("woke"), Some(MEMBER))
        );
        assert_eq!(
            wakes[0]["key"],
            hash16(&[MEMBER, "2026-11-10", "ready plan tasks t1 ready, t2 ready"])
        );
        assert_eq!(
            wake(&rig, false),
            format!("skipped: {MEMBER} already woken today for this work")
        );
        assert_eq!(
            rig.fake.sent.borrow().len(),
            2,
            "a retried tick never submits twice"
        );
        // The JSON receipt counts a typed wake as a wake.
        *rig.fake.ready.borrow_mut() = "t3 ready\n".into();
        let out = wake_json_at(
            false,
            &rig.dir.0,
            &rig.host,
            &rig.fake,
            &rig.stop,
            at(T),
            None,
        );
        let receipt: Value = serde_json::from_slice(&out.1).unwrap();
        assert!(
            receipt["status"].as_str().unwrap().starts_with("woke "),
            "{receipt}"
        );
        assert_eq!(
            (
                receipt["wake"].as_i64(),
                receipt["wakes"].as_array().map(Vec::len)
            ),
            (Some(1), Some(2))
        );
        let scheduled = wake_json_at(
            true,
            &rig.dir.0,
            &rig.host,
            &rig.fake,
            &rig.stop,
            at(T),
            None,
        );
        assert!(
            String::from_utf8(scheduled.1)
                .unwrap()
                .contains("\"wake\":0"),
            "a scheduled-only tick never types into a main"
        );
    }
    #[test]
    fn a_prompt_that_is_not_provably_empty_is_skipped_recorded_and_never_typed_into() {
        let rig = typed_rig();
        for screen in [
            "╭────╮\n│ ❯ draft │\n╰────╯\n",
            "╭────╮\n│ ❯ \x1b[90mdraft\x1b[0m\x1b[7m \x1b[0m │\n╰────╯\n",
            "❯ \x1b[2mdraft\x1b[22m\x1b[7m \x1b[0m\n",
            "❯  \x1b[7m \x1b[0m\n",
            "plain output\n",
            "❯ one\n❯ two\n",
            "❯ \nmore text\n",
        ] {
            *rig.fake.screen.borrow_mut() = screen.into();
            rig.dir.put(".oms/tick/last.json", "{\"wakes\":[]}");
            let status = wake(&rig, false);
            assert!(
                status.starts_with(&format!("skipped: {MEMBER}; ")),
                "{screen:?}: {status}"
            );
            assert!(
                rig.fake.sent.borrow().is_empty(),
                "{screen:?}: nothing typed, cleared or entered"
            );
            let wakes = rig.dir.wakes();
            assert_eq!(
                (wakes.len(), wakes[0]["outcome"].as_str()),
                (1, Some("skipped")),
                "{screen:?}"
            );
            assert!(wakes[0]["reason"].as_str().is_some_and(|r| !r.is_empty()));
        }
        *rig.fake.screen.borrow_mut() = "╭────╮\n│ ❯ draft │\n╰────╯\n".into();
        rig.dir.put(".oms/tick/last.json", "{\"wakes\":[]}");
        assert_eq!(
            wake(&rig, true),
            format!("would skip {MEMBER}: main input line holds text")
        );
        assert_eq!(rig.dir.wakes().len(), 0, "a dry run records nothing");
        // A recorded skip holds neither the dedupe key nor the daily cap.
        *rig.fake.screen.borrow_mut() = "╭────╮\n│ ❯ │\n╰────╯\n".into();
        rig.seed(json!([wake_row("2026-11-10", MEMBER, "x", "skipped")]));
        assert!(wake(&rig, false).starts_with("woke "));
        // A user draft appearing after the initial empty-input capture (while recording the
        // wake) must abort submission before typing anything and revert "woke" to "skipped".
        let late_draft = typed_rig();
        late_draft.fake.change_screen_after.set(1);
        *late_draft.fake.changed_screen.borrow_mut() = "╭────╮\n│ ❯ late draft │\n╰────╯\n".into();
        assert_eq!(
            wake(&late_draft, false),
            format!("skipped: {MEMBER}; main input line holds text")
        );
        assert!(late_draft.fake.sent.borrow().is_empty());
        let wakes = late_draft.dir.wakes();
        assert_eq!(
            (
                wakes.len(),
                wakes[0]["outcome"].as_str(),
                wakes[0]["reason"].as_str(),
            ),
            (1, Some("skipped"), Some("main input line holds text"))
        );
    }
    #[test]
    fn a_pane_that_changes_hands_before_typing_is_skipped_and_not_counted_as_woken() {
        let rig = typed_rig();
        // Stamps are read for the judgment (1st), then again right before typing (2nd).
        rig.fake.change_stamp_at.set(2);
        let status = wake(&rig, false);
        assert_eq!(
            status,
            format!("skipped: {MEMBER}; pane changed hands before input; nothing typed")
        );
        assert!(rig.fake.sent.borrow().is_empty());
        let wakes = rig.dir.wakes();
        assert_eq!(
            (wakes.len(), wakes[0]["outcome"].as_str()),
            (1, Some("skipped")),
            "the woke row is replaced by the skip"
        );
        // When typing starts and a subsequent send-keys (e.g. Enter) fails, the durable
        // "woke" row stays spent so a follow-up wake cannot duplicate the prompt.
        let partial = typed_rig();
        partial.fake.fail_send_at.set(2);
        let status = wake(&partial, false);
        assert_eq!(status, format!("skipped: {MEMBER}; send-keys failed"));
        let wakes = partial.dir.wakes();
        assert_eq!(
            (wakes.len(), wakes[0]["outcome"].as_str()),
            (1, Some("woke"))
        );
        assert_eq!(
            wake(&partial, false),
            format!("skipped: {MEMBER} already woken today for this work")
        );
        // When the panel lock is replaced after writing the woke row and before pre-input fallback,
        // it must report that the recorded wake remains rather than claiming last.json was untouched.
        let lock_swap = typed_rig();
        lock_swap.fake.change_stamp_at.set(2);
        lock_swap.fake.replace_artifacts_at_stamp.set(2);
        let status = wake(&lock_swap, false);
        assert!(
            status.starts_with(&format!(
                "skipped: {MEMBER}; pane changed hands before input; nothing typed (recorded wake kept:"
            )),
            "{status}"
        );
        assert!(lock_swap.fake.sent.borrow().is_empty());
        let wakes = lock_swap.dir.wakes();
        assert_eq!(
            (wakes.len(), wakes[0]["outcome"].as_str()),
            (1, Some("woke"))
        );
        // Replacing .oms/artifacts during the final capture-pane check (while the stamp remains
        // unchanged) must still abort typing and keep the recorded wake untouched.
        let cap_swap = typed_rig();
        cap_swap.fake.replace_artifacts_at_capture.set(2);
        let cap_status = wake(&cap_swap, false);
        assert!(
            cap_status.starts_with(&format!(
                "skipped: {MEMBER}; panel lock changed before input; nothing typed (recorded wake kept:"
            )),
            "{cap_status}"
        );
        assert!(cap_swap.fake.sent.borrow().is_empty());
        assert!(
            std::fs::read_dir(cap_swap.dir.0.join(".oms/artifacts"))
                .unwrap()
                .next()
                .is_none()
        );
        let wakes = cap_swap.dir.wakes();
        assert_eq!(
            (wakes.len(), wakes[0]["outcome"].as_str()),
            (1, Some("woke"))
        );
    }
    #[test]
    fn the_wake_needs_an_idle_childless_verified_main_and_waiting_work() {
        let rig = typed_rig();
        rig.idle_since(T, 1799);
        assert_eq!(
            wake(&rig, false),
            "skipped: main is not idle for 30 minutes"
        );
        rig.idle_since(T, 1800);
        assert!(
            wake(&rig, true).starts_with("would wake "),
            "exactly 30 minutes idles"
        );
        rig.dir
            .put(&format!(".oms/hooks/panel-activity/{MEMBER}.json"), "{}");
        assert_eq!(
            wake(&rig, false),
            "skipped: main is not idle for 30 minutes",
            "no idle record is not proof"
        );
        rig.idle_since(T, 3600);
        rig.fake.children.borrow_mut().push(json!({"attempt_id": "att_c", "terminal": false, "refs": {"panel_room_id": ROOM, "panel_role": "worker"}}));
        assert_eq!(wake(&rig, false), "skipped: main has a live child call");
        rig.fake.children.borrow_mut().clear();
        *rig.fake.ready.borrow_mut() = String::new();
        assert_eq!(
            wake(&rig, false),
            "skipped: no ready plan work, pending proposal or native return"
        );
        // A proposal held for a GitHub issue never wakes the main and records nothing.
        rig.dir.put(
            ".oms/plan/autopilot-run.json",
            "{\"stage\":\"proposal-review\",\"blocked\":\"github-issue\"}",
        );
        let ledger = || std::fs::read(rig.dir.0.join(".oms/tick/last.json")).ok();
        let before = ledger();
        for _ in 0..2 {
            assert_eq!(
                wake(&rig, false),
                format!("skipped: {ISSUE_PENDING}; resumes when {ISSUE_RESTAMP}")
            );
        }
        assert!(rig.fake.sent.borrow().is_empty());
        assert_eq!(ledger(), before);
        *rig.fake.ready.borrow_mut() = "t1 ready\n".into();
        let held = wake(&rig, true);
        assert!(
            held.contains("ready plan tasks t1 ready") && !held.contains("autopilot proposal"),
            "{held}"
        );
        *rig.fake.ready.borrow_mut() = String::new();
        rig.dir.put(
            ".oms/plan/autopilot-run.json",
            "{\"stage\":\"proposal-review\"}",
        );
        assert!(wake(&rig, true).contains("an autopilot proposal waits in proposal-review"));
        *rig.fake.ready.borrow_mut() = "t1 ready\n".into();
        assert!(
            wake(&rig, true).contains("ready plan tasks t1 ready; an autopilot proposal waits")
        );
        // At the daily cap the typed wake stops too.
        rig.seed(json!(
            (0..WAKE_DAILY_LIMIT)
                .map(|i| wake_row("2026-11-10", "call-other", &format!("k{i}"), "woke"))
                .collect::<Vec<_>>()
        ));
        assert_eq!(
            wake(&rig, false),
            format!("skipped: {MEMBER} reached 6 wakes today")
        );
        assert!(rig.fake.sent.borrow().is_empty());
    }
    #[test]
    fn a_main_without_a_pane_to_type_into_names_why() {
        let rig = Rig::new("driving");
        rig.fake.live_main();
        rig.fake.no_session.set(true);
        let status = wake(&rig, false);
        assert!(
            status.starts_with("skipped: no live main pane to type into (")
                && status.contains("oms panel --release-main"),
            "{status}"
        );
    }

    #[test]
    fn files_other_writers_leave_under_a_group_writable_umask_are_accepted() {
        use std::os::unix::fs::PermissionsExt;
        let rig = typed_rig();
        // The activity hook and the sweep write under the user's umask, not 0600/0700.
        let loosen = |relative: &str, mode: u32| {
            std::fs::set_permissions(
                rig.dir.0.join(relative),
                std::fs::Permissions::from_mode(mode),
            )
            .unwrap();
        };
        for dir in [".", ".oms", ".oms/hooks", ".oms/hooks/panel-activity"] {
            loosen(dir, 0o775);
        }
        loosen(&format!(".oms/hooks/panel-activity/{MEMBER}.json"), 0o664);
        rig.dir
            .put(".oms/tick/last.json", "{\"schema\":1,\"wakes\":[]}");
        loosen(".oms/tick", 0o775);
        loosen(".oms/tick/last.json", 0o664);
        let status = wake(&rig, false);
        assert!(status.starts_with("woke "), "{status}");
        assert_eq!(rig.dir.wakes().len(), 1);
        // The resume path rewrites the same loose receipt.
        let resume = Rig::new("driving");
        resume
            .dir
            .put(".oms/tick/last.json", "{\"schema\":1,\"wakes\":[]}");
        loosen_in(&resume.dir, ".", 0o775);
        loosen_in(&resume.dir, ".oms", 0o775);
        loosen_in(&resume.dir, ".oms/tick", 0o775);
        loosen_in(&resume.dir, ".oms/tick/last.json", 0o664);
        assert!(
            resume
                .tick("2026-11-10T09:00:00Z", false, None)
                .starts_with("resumed autopilot")
        );
        assert_eq!(resume.dir.wakes().len(), 1);
    }
    fn loosen_in(dir: &Dir, relative: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.0.join(relative), std::fs::Permissions::from_mode(mode))
            .unwrap();
    }
    #[test]
    fn a_held_panel_lock_defers_writers_and_never_blocks_a_list() {
        let rig = Rig::new("driving");
        let t = "2026-11-10T09:00:00Z";
        rig.schedule(t, &["--contract", OWNER, "--every", "1"]);
        let before = rig.log();
        let store = Store::open(&rig.dir.0).unwrap();
        let _main_start = store
            .transaction("panel-main-start", Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            rig.tick("2026-11-10T10:30:00Z", true, None),
            "skipped: panel typing lock held"
        );
        assert_eq!(
            rig.tick("2026-11-10T10:30:00Z", false, None),
            "skipped: panel typing lock held"
        );
        assert!(
            rig.host.launched.borrow().is_empty() && !rig.dir.tick("last.json").exists(),
            "a busy lock leaves no ledger row"
        );
        let busy = rig.schedule_wait(
            t,
            &["--pause", "--contract", OWNER],
            Duration::from_millis(500),
        );
        assert!(
            busy.0 == 1 && busy.2.contains("panel typing lock held"),
            "{busy:?}"
        );
        assert_eq!(rig.log(), before, "nothing was appended without the lock");
        assert_eq!(
            rig.schedule_wait(t, &["--list"], Duration::from_millis(500))
                .0,
            0
        );
        // A dry run proves liveness without the lock.
        assert!(
            rig.env("2026-11-10T10:30:00Z")
                .resume(true, None, true)
                .starts_with("would resume autopilot")
        );
        // Typed wakes defer the same way.
        let typed = typed_rig();
        let held = Store::open(&typed.dir.0).unwrap();
        let _tx = held
            .transaction("panel-main-start", Duration::from_secs(1))
            .unwrap();
        assert_eq!(wake(&typed, false), "skipped: panel typing lock held");
        assert!(typed.fake.sent.borrow().is_empty());
    }

    #[test]
    fn the_sweep_marker_limits_one_tick_run_to_one_resume_across_repos() {
        let (first, second) = (Rig::new("driving"), Rig::new("driving"));
        let marker = first.dir.0.join("run-marker");
        let status = |rig: &Rig| {
            let out = wake_idle_main_json_once(
                false,
                &rig.dir.0,
                &rig.host,
                &rig.fake,
                &rig.stop,
                Some(&marker),
            );
            serde_json::from_slice::<Value>(&out.1).unwrap()
        };
        let a = status(&first);
        assert_eq!(a["wake"], 1, "{a}");
        let b = status(&second);
        assert_eq!(b["wake"], 0);
        assert!(
            b["status"]
                .as_str()
                .unwrap()
                .contains("this tick run already resumed one autopilot"),
            "{b}"
        );
        assert!(second.host.launched.borrow().is_empty());
    }

    // ---- input-line proof

    #[test]
    fn the_input_line_proof_is_conservative() {
        let dim = "\x1b[2m";
        let reset = "\x1b[0m";
        let cases: [(&str, &str, bool); 16] = [
            ("empty box", "╭────╮\n│ ❯ │\n╰────╯\n", true),
            ("bare empty prompt", "output\n❯ \n", true),
            (
                "lone block cursor at column zero",
                "╭────╮\n│ ❯ \x1b[7m \x1b[0m │\n╰────╯\n",
                true,
            ),
            (
                "dim placeholder",
                &format!("╭────╮\n│ ❯ {dim}Try something{reset} │\n╰────╯\n"),
                true,
            ),
            (
                "grey placeholder",
                "╭────╮\n│ ❯ \x1b[90mAsk anything\x1b[0m │\n╰────╯\n",
                true,
            ),
            (
                "cursor before placeholder",
                &format!("╭────╮\n│ ❯ \x1b[7mT\x1b[0m{dim}ry it{reset} │\n╰────╯\n"),
                true,
            ),
            ("typed text", "╭────╮\n│ ❯ hello │\n╰────╯\n", false),
            (
                "cursor after text",
                "╭────╮\n│ ❯ hi\x1b[7m \x1b[0m│\n╰────╯\n",
                false,
            ),
            (
                "grey draft with drawn cursor",
                "╭────╮\n│ ❯ \x1b[90mdraft\x1b[0m\x1b[7m \x1b[0m │\n╰────╯\n",
                false,
            ),
            (
                "dim draft with drawn cursor",
                &format!("❯ {dim}draft\x1b[22m\x1b[7m \x1b[0m\n"),
                false,
            ),
            (
                "typed spaces before drawn cursor",
                "❯  \x1b[7m \x1b[0m\n",
                false,
            ),
            ("no prompt", "just output\n", false),
            ("two prompts in the region", "❯ a\n❯ b\n", false),
            ("text under the prompt", "❯ \nmore\n", false),
            (
                "placeholder then typed text",
                &format!("❯ {dim}hint{reset}typed\n"),
                false,
            ),
            (
                "bold typed text is not placeholder",
                "❯ \x1b[1mtyped\x1b[0m\n",
                false,
            ),
        ];
        for (name, screen, empty) in cases {
            assert_eq!(screen_input_empty(screen).0, empty, "{name}");
        }
        assert_eq!(screen_input_empty("❯ x\n").1, "main input line holds text");
        assert_eq!(
            screen_input_empty("x\n").1,
            "no input prompt line visible in the main pane"
        );
        // Copy mode and an unreadable pane refuse without capturing.
        let rig = Rig::new("driving");
        assert_eq!(rig.env(T).input_line_empty(PANE), (true, String::new()));
    }

    // ---- status line

    #[test]
    fn the_summary_line_counts_spent_resumed_and_skipped_wakes() {
        let rows = vec![
            wake_row("2026-11-10", "m", "a", "woke"),
            wake_row("2026-11-10", "autopilot", "b", "resumed"),
            wake_row("2026-11-10", "autopilot", "c", "skipped"),
            wake_row("2026-11-09", "m", "d", "woke"),
        ];
        assert_eq!(
            wake_summary(&rows, "2026-11-10"),
            "wakes today (UTC): 2 of 6 spent (1 main prompt(s), 1 autopilot resume(s)); 1 skipped; tokens, cost and wall time: not recorded"
        );
        assert_eq!(
            wake_summary(&rows, "2026-11-12"),
            "wakes today (UTC): 0 of 6 spent; tokens, cost and wall time: not recorded"
        );
        let dir = Dir::new();
        let path = dir.tick("last.json");
        assert_eq!(
            wake_summary_cli(&path, at(T)),
            (0, vec![], vec![]),
            "no receipt: nothing to say"
        );
        dir.put(".oms/tick/last.json", &json!({"wakes": rows}).to_string());
        assert!(
            String::from_utf8(wake_summary_cli(&path, at(T)).1)
                .unwrap()
                .starts_with("wakes today (UTC): 2 of 6 spent")
        );
        dir.put(".oms/tick/last.json", "{");
        assert_eq!(
            String::from_utf8(wake_summary_cli(&path, at(T)).1).unwrap(),
            "wakes today (UTC): unknown (tick receipt is empty or not JSON)\n"
        );
        dir.put(".oms/tick/last.json", "{\"wakes\":[]}");
        assert_eq!(wake_summary_cli(&path, at(T)).1, Vec::<u8>::new());
    }
    #[test]
    fn wake_cli_parses_its_flags_and_reports_one_line() {
        let rig = Rig::new("driving");
        let run = |argv: &[&str]| {
            let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
            wake_cli(&argv, &rig.host, &rig.fake, &rig.stop)
        };
        let repo = rig.dir.0.to_str().unwrap();
        for bad in [&[][..], &[repo, "--once"], &[repo, "--bogus"]] {
            assert_eq!(run(bad).0, 2, "{bad:?}");
        }
        let dry = run(&[repo, "--dry-run", "--scheduled"]);
        assert!(
            dry.0 == 0
                && String::from_utf8(dry.1)
                    .unwrap()
                    .starts_with("skipped: no schedule binds contract "),
            "a scheduled tick resumes nothing a schedule does not make due"
        );
        let live = run(&[repo, "--dry-run"]);
        assert!(
            String::from_utf8(live.1)
                .unwrap()
                .starts_with("would resume autopilot")
        );
        rig.stop.store(true, Ordering::Release);
        assert_eq!(run(&[repo]).0, 130);
    }
}
