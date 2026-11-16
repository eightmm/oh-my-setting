//! `slurm-submit reconcile`: recover the scheduler's view of one ledger key.
//! Absence is never proven: no evidence yields `unknown` (status 75), so a
//! caller cannot blindly resubmit after an accounting lag or a lost reply.
use crate::ledger::{self, f, kind};
use crate::{json as j, scheduler, storage};
use oms_context::Json;
use oms_json::Value;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

pub(crate) struct Options {
    pub key: OsString,
    pub job_id: String,
    pub repo: OsString,
    pub json: bool,
}

pub(crate) fn parse(args: &[OsString]) -> Result<Option<Options>, Vec<u8>> {
    let mut options = Options {
        key: OsString::new(),
        job_id: String::new(),
        repo: OsString::new(),
        json: false,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_bytes();
        let (name, inline) = match arg.iter().position(|&b| b == b'=') {
            Some(at) if arg.starts_with(b"--") => (&arg[..at], Some(&arg[at + 1..])),
            _ => (arg, None),
        };
        match name {
            b"-h" | b"--help" => return Ok(None),
            b"--json" if inline.is_none() => options.json = true,
            b"--key" | b"--job-id" | b"--repo" => {
                let value = match inline {
                    Some(v) => OsString::from_vec(v.to_vec()),
                    None => {
                        index += 1;
                        args.get(index)
                            .ok_or_else(|| {
                                crate::option_error(b"", OsStr::from_bytes(name), b" needs a value")
                            })?
                            .clone()
                    }
                };
                match name {
                    b"--key" => options.key = value,
                    b"--repo" => options.repo = value,
                    _ => options.job_id = value.to_string_lossy().into_owned(),
                }
            }
            _ => {
                return Err(crate::option_error(
                    b"unknown reconcile option: ",
                    &args[index],
                    b"",
                ));
            }
        }
        index += 1;
    }
    if options.key.is_empty() == options.job_id.is_empty() {
        return Err(crate::fail(
            b"reconcile needs exactly one of --key KEY or --job-id ID",
        ));
    }
    if !options.job_id.is_empty() && !digits(&options.job_id) {
        return Err(crate::fail(b"--job-id must be a numeric Slurm job id"));
    }
    Ok(Some(options))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Class {
    Active,
    Succeeded,
    Failed,
    Oom,
    Preempted,
    Timeout,
    Cancelled,
    Unknown,
}
impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Active => "active",
            Class::Succeeded => "succeeded",
            Class::Failed => "failed",
            Class::Oom => "oom",
            Class::Preempted => "preempted",
            Class::Timeout => "timeout",
            Class::Cancelled => "cancelled",
            Class::Unknown => "unknown",
        }
    }
    fn code(self) -> i32 {
        match self {
            Class::Active | Class::Succeeded => 0,
            Class::Unknown => 75,
            _ => 1,
        }
    }
}

/// Slurm state text (`CANCELLED by 7`, `CANCELLED+`) to a conservative class.
/// Only a completed job with exit `0:0` is success; unrecognised text is unknown.
pub(crate) fn classify(state: &str, exit: Option<&str>) -> Class {
    let word = state
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('+')
        .to_ascii_uppercase();
    match word.as_str() {
        "PENDING" | "RUNNING" | "COMPLETING" | "CONFIGURING" | "REQUEUED" | "RESIZING"
        | "SUSPENDED" | "REQUEUE_HOLD" | "REQUEUE_FED" | "RESV_DEL_HOLD" | "SIGNALING"
        | "STAGE_OUT" | "STOPPED" | "SPECIAL_EXIT" => Class::Active,
        "COMPLETED" => match exit.map(str::trim) {
            None | Some("") | Some("0:0") => Class::Succeeded,
            Some(_) => Class::Failed,
        },
        "OUT_OF_MEMORY" => Class::Oom,
        "PREEMPTED" => Class::Preempted,
        "TIMEOUT" => Class::Timeout,
        "CANCELLED" | "REVOKED" => Class::Cancelled,
        "FAILED" | "NODE_FAIL" | "BOOT_FAIL" | "DEADLINE" => Class::Failed,
        _ => Class::Unknown,
    }
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub base: String,
    pub state: String,
    pub exit: Option<String>,
    text: String,
}
/// Outcome of one scheduler query; a failed query is no evidence at all.
#[derive(Debug)]
pub(crate) enum Query {
    Rows(Vec<Entry>),
    Failed(String),
}
impl Query {
    fn status(&self) -> String {
        match self {
            Query::Rows(_) => "ok".into(),
            Query::Failed(why) => format!("failed: {why}"),
        }
    }
}

// `123_4` (array task) and `123+0` (heterogeneous component) belong to job 123.
fn base_id(id: &str) -> Option<String> {
    let base = id.split(['_', '+', '.']).next().unwrap_or("");
    digits(base).then(|| base.to_owned())
}
fn state_text(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" _+".contains(&b))
}
/// squeue `%i|%T|%j|%k`: free text (name, comment) comes last.
pub(crate) fn parse_squeue(raw: &[u8]) -> Vec<Entry> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let (id, state, text) = (parts.next()?, parts.next()?, parts.next()?);
            state_text(state).then_some(())?;
            Some(Entry {
                base: base_id(id.trim())?,
                state: state.into(),
                exit: None,
                text: text.into(),
            })
        })
        .collect()
}
/// sacct `JobIDRaw|JobName|State|ExitCode|Comment`; a `|` inside free text is
/// tolerated by locating the `N:N` exit code instead of counting fields.
pub(crate) fn parse_sacct(raw: &[u8]) -> Vec<Entry> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('|').collect();
            let at = (2..fields.len()).find(|&i| {
                let mut n = fields[i].split(':');
                matches!((n.next(), n.next(), n.next()), (Some(a), Some(b), None)
                    if digits(a) && digits(b))
            })?;
            let state = fields[at - 1];
            state_text(state).then_some(())?;
            Some(Entry {
                base: base_id(fields[0].trim())?,
                state: state.into(),
                exit: Some(fields[at].into()),
                text: [&fields[1..at - 1], &fields[at + 1..]].concat().join("|"),
            })
        })
        .collect()
}

// The tag ends where the 12 character digest ends, so a longer token never matches.
fn tagged(text: &str, short: &str) -> bool {
    let needle = format!("oms-key={short}");
    text.match_indices(&needle).any(|(at, _)| {
        !text[at + needle.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

pub(crate) enum Matcher<'a> {
    Job(&'a str),
    Tag(&'a str),
    /// No identifier can select a job (untagged attempt, no job id).
    Nothing,
}
impl Matcher<'_> {
    fn selects(&self, entry: &Entry) -> bool {
        match self {
            Matcher::Job(id) => entry.base == *id,
            Matcher::Tag(short) => tagged(&entry.text, short),
            Matcher::Nothing => false,
        }
    }
}

pub(crate) struct Verdict {
    pub class: Class,
    pub job_id: Option<String>,
    pub source: &'static str,
    pub reason: String,
    pub exit: Option<String>,
}
fn unknown(reason: impl Into<String>) -> Verdict {
    Verdict {
        class: Class::Unknown,
        job_id: None,
        source: "",
        reason: reason.into(),
        exit: None,
    }
}
fn aggregate(entries: &[&Entry]) -> (Class, Option<String>) {
    let classes: Vec<Class> = entries
        .iter()
        .map(|e| classify(&e.state, e.exit.as_deref()))
        .collect();
    let exit = entries.iter().find_map(|e| e.exit.clone());
    for wanted in [Class::Unknown, Class::Active] {
        if classes.contains(&wanted) {
            return (wanted, exit);
        }
    }
    for wanted in [
        Class::Failed,
        Class::Oom,
        Class::Timeout,
        Class::Preempted,
        Class::Cancelled,
    ] {
        if classes.contains(&wanted) {
            return (wanted, exit);
        }
    }
    (Class::Succeeded, exit)
}

/// Combine both queries. A live squeue row beats a terminal sacct row (a requeue
/// keeps accounting history); two distinct jobs for one selector are ambiguous.
pub(crate) fn resolve(squeue: &Query, sacct: &Query, matcher: &Matcher<'_>) -> Verdict {
    fn pick<'q>(q: &'q Query, matcher: &Matcher<'_>) -> Vec<&'q Entry> {
        match q {
            Query::Rows(rows) => rows.iter().filter(|e| matcher.selects(e)).collect(),
            Query::Failed(_) => Vec::new(),
        }
    }
    let (live, accounted) = (pick(squeue, matcher), pick(sacct, matcher));
    let mut bases: Vec<&str> = live
        .iter()
        .chain(accounted.iter())
        .map(|e| e.base.as_str())
        .collect();
    bases.sort_unstable();
    bases.dedup();
    let describe = |q: &Query, name: &str| format!("{name} {}", q.status());
    match bases.as_slice() {
        [] => unknown(format!(
            "no matching job in squeue or sacct ({}; {}); accounting may lag or the job may \
             not exist, which cannot be told apart",
            describe(squeue, "squeue"),
            describe(sacct, "sacct")
        )),
        [one] => {
            let (source, chosen) = if live
                .iter()
                .any(|e| classify(&e.state, None) == Class::Active)
            {
                ("squeue", live)
            } else if !accounted.is_empty() {
                ("sacct", accounted)
            } else {
                ("squeue", live)
            };
            let (class, exit) = aggregate(&chosen);
            let reason = if class == Class::Unknown {
                "scheduler reported a state that is not recognised".into()
            } else {
                String::new()
            };
            Verdict {
                class,
                job_id: Some((*one).to_owned()),
                source,
                reason,
                exit,
            }
        }
        _ => unknown(format!("ambiguous: jobs {} all match", bases.join(", "))),
    }
}

enum Failure {
    Interrupted,
    Unavailable(String),
}
struct Capped(Vec<u8>);
impl Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > OUTPUT_LIMIT {
            return Err(std::io::Error::other("scheduler output limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn timeout() -> Duration {
    let seconds = std::env::var("OMS_SLURM_QUERY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30);
    Duration::from_secs(seconds.clamp(1, 300))
}
fn run_query(
    variable: &str,
    default: &str,
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
) -> Result<Vec<u8>, Failure> {
    let program = std::env::var_os(variable)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into());
    let (watchdog, done) = (AtomicBool::new(false), AtomicBool::new(false));
    let limit = timeout();
    let mut out = Capped(Vec::new());
    let execution = std::thread::scope(|scope| {
        scope.spawn(|| {
            let start = Instant::now();
            while !done.load(Ordering::Acquire) {
                if interrupted.load(Ordering::Relaxed) || start.elapsed() >= limit {
                    watchdog.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let result = scheduler::run(
            &program,
            args,
            cwd,
            scheduler::Input::Closed,
            &watchdog,
            &mut out,
            &mut std::io::sink(),
        );
        done.store(true, Ordering::Release);
        result
    });
    match execution {
        scheduler::Execution::Interrupted if interrupted.load(Ordering::Relaxed) => {
            Err(Failure::Interrupted)
        }
        scheduler::Execution::Interrupted => Err(Failure::Unavailable("timed out".into())),
        scheduler::Execution::Unknown => Err(Failure::Unavailable("process lost".into())),
        scheduler::Execution::Answered {
            output_complete: false,
            ..
        } => Err(Failure::Unavailable(
            "output incomplete or too large".into(),
        )),
        scheduler::Execution::Answered { code: 0, .. } => Ok(out.0),
        scheduler::Execution::Answered { code, .. } => {
            Err(Failure::Unavailable(format!("exit {code}")))
        }
    }
}

// sacct reads `-S` in the caller's local time while ledger stamps are UTC;
// two days of margin covers every offset and a start-time skew.
fn window(ts: &str) -> String {
    let start = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%SZ")
        .ok()
        .and_then(|t| t.checked_sub_signed(chrono::Duration::days(2)))
        .unwrap_or_else(|| {
            chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                .naive_utc()
                .checked_sub_signed(chrono::Duration::days(7))
                .unwrap_or_default()
        });
    start.format("%Y-%m-%dT%H:%M:%S").to_string()
}
fn user() -> Option<OsString> {
    ["USER", "LOGNAME"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| {
            !v.is_empty()
                && v.as_bytes()
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        })
}

struct Target {
    key: OsString,
    attempt: Option<Value>,
    attempt_id: Vec<u8>,
    job_id: Option<String>,
    short: Option<String>,
    start: String,
    provenance: Vec<(&'static str, Value)>,
}
fn text_of(row: &Json, name: &str) -> Option<String> {
    match f(row, name) {
        Some(Json::String(s)) if !s.is_empty() => Some(String::from_utf8_lossy(s).into_owned()),
        Some(Json::Number(n)) => Some(n.clone()),
        _ => None,
    }
}
/// The ledger's authoritative view of `key`, using the same window rules as
/// `slurm-submit`; only an unresolved or submitted attempt can be reconciled.
fn target(rows: &[Json], key: &OsStr) -> Result<Option<Target>, String> {
    let state = ledger::state(rows, key)?;
    let Some(row) = state.row else {
        return Ok(None);
    };
    let attempt = f(row, "attempt");
    let wanted = ledger::identity(attempt)?;
    let intent = ledger::rows_for(rows, key).into_iter().rev().find(|r| {
        kind(r, "intent") && ledger::identity(f(r, "attempt")).ok().as_ref() == Some(&wanted)
    });
    let short = intent.and_then(|i| {
        if f(i, "tag") == Some(&Json::Bool(false)) {
            return None;
        }
        Some(if text_of(i, "key_source").as_deref() == Some("custom") {
            ledger::digest(key.as_bytes())[..12].to_owned()
        } else {
            String::from_utf8_lossy(key.as_bytes())
                .chars()
                .take(12)
                .collect()
        })
    });
    let provenance = intent
        .map(|i| {
            ledger::PROVENANCE_FIELDS
                .iter()
                .filter_map(|n| f(i, n).map(|v| (*n, j::copy(Some(v)))))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(Target {
        key: key.to_owned(),
        attempt: attempt.map(|a| j::copy(Some(a))),
        attempt_id: wanted,
        job_id: (state.name == "submitted")
            .then(|| text_of(row, "job_id"))
            .flatten(),
        short,
        start: window(&text_of(intent.unwrap_or(row), "ts").unwrap_or_default()),
        provenance,
    }))
}

fn last_reconciled(rows: &[Json], target: &Target) -> Option<(String, String)> {
    ledger::rows_for(rows, &target.key)
        .into_iter()
        .rev()
        .find(|r| {
            kind(r, "reconciled")
                && ledger::identity(f(r, "attempt")).ok().as_ref() == Some(&target.attempt_id)
        })
        .map(|r| {
            (
                text_of(r, "state").unwrap_or_default(),
                text_of(r, "job_id").unwrap_or_default(),
            )
        })
}

fn record(
    ledger_path: &Path,
    logical: &Path,
    old: &Target,
    verdict: &Verdict,
    interrupted: &AtomicBool,
) -> Result<bool, String> {
    let Some(job) = &verdict.job_id else {
        return Ok(false);
    };
    let guard = ledger::acquire(ledger_path, logical, interrupted)?;
    let rows = storage::load(ledger_path, logical).map_err(str::to_owned)?;
    let Some(current) = target(&rows, &old.key)? else {
        return Ok(false);
    };
    // A concurrent forced resubmit or result row changed what this attempt is.
    if current.attempt_id != old.attempt_id || current.job_id.is_some() != old.job_id.is_some() {
        return Ok(false);
    }
    let base = |kind: &str| {
        let mut row = j::object([
            ("schema", j::integer(1)),
            ("type", j::text(kind)),
            ("key", j::os(&old.key)),
            (
                "attempt",
                old.attempt.as_ref().map_or(Value::Null, j::copy_value),
            ),
            ("ts", j::text(&ledger::now().unwrap_or_default())),
            ("job_id", j::text(job)),
            ("reconciled", Value::Bool(true)),
            ("source", j::text(verdict.source)),
        ]);
        for (name, value) in &old.provenance {
            j::put(&mut row, name, j::copy_value(value));
        }
        row
    };
    let mut lines = Vec::new();
    if old.job_id.is_none() {
        let mut row = base("submitted");
        j::put(&mut row, "cluster_name", j::text(""));
        lines.extend(ledger::line(&row));
    }
    let now = (verdict.class.name().to_owned(), job.clone());
    if last_reconciled(&rows, old).as_ref() != Some(&now) {
        let mut row = base("reconciled");
        j::put(&mut row, "state", j::text(verdict.class.name()));
        if let Some(exit) = &verdict.exit {
            j::put(&mut row, "scheduler_exit", j::text(exit));
        }
        lines.extend(ledger::line(&row));
    }
    if lines.is_empty() {
        return Ok(false);
    }
    if interrupted.load(Ordering::Relaxed) {
        return Err("interrupted".into());
    }
    guard.check().map_err(|e| e.to_string())?;
    storage::append(ledger_path, logical, &lines).map_err(str::to_owned)?;
    Ok(true)
}

fn submitted_key(rows: &[Json], job: &str) -> Option<OsString> {
    rows.iter().rev().find_map(|r| {
        (kind(r, "submitted") && text_of(r, "job_id").as_deref() == Some(job))
            .then(|| match f(r, "key") {
                Some(Json::String(k)) => Some(OsString::from_vec(k.clone())),
                _ => None,
            })
            .flatten()
    })
}

pub(crate) fn run(
    options: Options,
    cwd: &Path,
    supplied: Option<&crate::repository::Repository>,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
) -> Result<i32, String> {
    let logical = crate::repository::caller_spelling(cwd)?;
    let base = if options.repo.is_empty() {
        logical.clone()
    } else {
        crate::supplied_path(&logical, &options.repo)
    };
    let discovered;
    let repository = match supplied {
        Some(r) if options.repo.is_empty() => r,
        _ => {
            discovered = crate::repository::discover_with_interrupt(&base, interrupted)?;
            &discovered
        }
    };
    let ledger_path = crate::ledger_path(&base, repository);
    storage::validate_path(&ledger_path).map_err(str::to_owned)?;
    let rows = storage::load(&ledger_path, &base).map_err(str::to_owned)?;
    let key = if options.key.is_empty() {
        submitted_key(&rows, &options.job_id)
    } else {
        Some(options.key.clone())
    };
    let found = match &key {
        Some(key) => target(&rows, key)?,
        None => None,
    };
    if found.is_none() && options.job_id.is_empty() {
        return Err("no submitted or unresolved ledger record for this key".into());
    }
    let job = (!options.job_id.is_empty())
        .then(|| options.job_id.clone())
        .or_else(|| found.as_ref().and_then(|t| t.job_id.clone()));
    let matcher = match (&job, found.as_ref().and_then(|t| t.short.as_deref())) {
        (Some(id), _) => Matcher::Job(id),
        (None, Some(short)) => Matcher::Tag(short),
        (None, None) => Matcher::Nothing,
    };
    let start = found
        .as_ref()
        .map_or_else(|| window(""), |t| t.start.clone());
    let who = user();
    let mut squeue_args: Vec<OsString> = ["-h", "-o", "%i|%T|%j|%k"].map(Into::into).to_vec();
    let mut sacct_args: Vec<OsString> = ["-X", "--parsable2", "-n", "-S"].map(Into::into).to_vec();
    sacct_args.push(start.into());
    sacct_args.push("--format=JobIDRaw,JobName,State,ExitCode,Comment".into());
    match (&job, who) {
        (Some(id), _) => {
            for args in [&mut squeue_args, &mut sacct_args] {
                args.push("-j".into());
                args.push(id.into());
            }
        }
        (None, Some(who)) => {
            for args in [&mut squeue_args, &mut sacct_args] {
                args.push("-u".into());
                args.push(who.clone());
            }
        }
        (None, None) => {}
    }
    let query =
        |variable, default, args: &[OsString], parse: fn(&[u8]) -> Vec<Entry>| match run_query(
            variable,
            default,
            args,
            &base,
            interrupted,
        ) {
            Ok(raw) => Ok(Query::Rows(parse(&raw))),
            Err(Failure::Interrupted) => Err("interrupted".to_owned()),
            Err(Failure::Unavailable(why)) => Ok(Query::Failed(why)),
        };
    let squeue = query("OMS_SLURM_SQUEUE_BIN", "squeue", &squeue_args, parse_squeue)?;
    let sacct = query("OMS_SLURM_SACCT_BIN", "sacct", &sacct_args, parse_sacct)?;
    let verdict = resolve(&squeue, &sacct, &matcher);
    let recorded = match &found {
        Some(t) if verdict.class != Class::Unknown => {
            record(&ledger_path, &base, t, &verdict, interrupted)?
        }
        _ => false,
    };
    let attempt = found
        .as_ref()
        .and_then(|t| t.attempt.as_ref())
        .map_or_else(String::new, |a| {
            String::from_utf8_lossy(&j::encode(a, true))
                .trim_matches('"')
                .to_owned()
        });
    let shown_key = key
        .as_ref()
        .map_or_else(String::new, |k| k.to_string_lossy().into_owned());
    let code = verdict.class.code();
    let line = if options.json {
        let value = j::object([
            ("state", j::text(verdict.class.name())),
            ("exit", j::integer(i64::from(code))),
            ("key", j::text(&shown_key)),
            ("attempt", j::text(&attempt)),
            ("job_id", j::text(verdict.job_id.as_deref().unwrap_or(""))),
            ("source", j::text(verdict.source)),
            ("recorded", Value::Bool(recorded)),
            (
                "recovered",
                Value::Bool(recorded && found.as_ref().is_some_and(|t| t.job_id.is_none())),
            ),
            (
                "scheduler_exit",
                j::text(verdict.exit.as_deref().unwrap_or("")),
            ),
            ("reason", j::text(&verdict.reason)),
            ("squeue", j::text(&squeue.status())),
            ("sacct", j::text(&sacct.status())),
        ]);
        ledger::line(&value)
    } else {
        let mut text = format!(
            "reconcile: {} job {} source {} ledger {}\n",
            verdict.class.name(),
            verdict.job_id.as_deref().unwrap_or("-"),
            if verdict.source.is_empty() {
                "-"
            } else {
                verdict.source
            },
            if recorded { "updated" } else { "unchanged" }
        );
        if verdict.class == Class::Unknown {
            text.push_str(&format!(
                "{}\nnot resubmitting: confirm with squeue/sacct before --force-resubmit\n",
                verdict.reason
            ));
        }
        text.into_bytes()
    };
    out.write_all(&line)
        .and_then(|()| out.flush())
        .map_err(|_| "cannot write command output".to_owned())?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(squeue: &str, sacct: &str, matcher: &Matcher<'_>) -> Verdict {
        resolve(
            &Query::Rows(parse_squeue(squeue.as_bytes())),
            &Query::Rows(parse_sacct(sacct.as_bytes())),
            matcher,
        )
    }

    #[test]
    fn array_and_heterogeneous_ids_collapse_to_one_job() {
        let v = verdict(
            "77_1|RUNNING|n|oms-key=abc\n77_2|PENDING|n|oms-key=abc\n77+0|RUNNING|n|oms-key=abc\n",
            "",
            &Matcher::Tag("abc"),
        );
        assert_eq!((v.class, v.job_id.as_deref()), (Class::Active, Some("77")));
    }

    #[test]
    fn live_row_beats_terminal_history_and_a_longer_tag_never_matches() {
        let v = verdict(
            "5|PENDING|n|oms-key=abc\n",
            "5|n|PREEMPTED|0:0|oms-key=abc\n",
            &Matcher::Tag("abc"),
        );
        assert_eq!((v.class, v.source), (Class::Active, "squeue"));
        let v = verdict("", "5|n|COMPLETED|0:0|oms-key=abcd\n", &Matcher::Tag("abc"));
        assert_eq!(v.class, Class::Unknown);
    }

    #[test]
    fn pipes_inside_free_text_do_not_hide_the_state() {
        let v = verdict(
            "",
            "9|a|b|TIMEOUT|0:15|oms-key=abc|x\n",
            &Matcher::Tag("abc"),
        );
        assert_eq!((v.class, v.job_id.as_deref()), (Class::Timeout, Some("9")));
    }

    #[test]
    fn completed_requires_a_zero_exit_code() {
        assert_eq!(classify("COMPLETED", Some("0:0")), Class::Succeeded);
        assert_eq!(classify("COMPLETED", Some("0:9")), Class::Failed);
        assert_eq!(classify("CANCELLED+", None), Class::Cancelled);
        assert_eq!(classify("", None), Class::Unknown);
    }
}
