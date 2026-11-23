//! Complete native active-task lifecycle over the existing Markdown records.
//! The parent CLI supplies reference ownership and native journal/memory
//! producers through [`Services`]. There is no legacy backend dispatch.
#![forbid(unsafe_code)]

pub mod assignment;
mod cli;
pub mod consumers;
mod git;
pub mod json;
pub mod lifecycle;
pub mod observe;
pub mod packet;
mod process;
pub mod projection;
pub mod services;
mod storage;

use chrono::{DateTime, Utc};
pub use git::{command_hash, state_fingerprint};
use oms_context::Json;
use oms_safe_fs::Root;
pub use services::Services;
use services::*;
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub struct Error {
    pub code: i32,
    pub message: Vec<u8>,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(code: i32, message: impl AsRef<str>) -> Self {
        Self::bytes(code, message.as_ref().as_bytes().to_vec())
    }
    pub fn bytes(code: i32, message: Vec<u8>) -> Self {
        Self { code, message }
    }
    pub fn service(name: &str) -> Self {
        Self::new(
            1,
            format!("native {name} service is unavailable; parent CLI injection is required"),
        )
    }
    fn io(op: &str, e: impl fmt::Display) -> Self {
        Self::new(1, format!("{op}: {e}"))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message))
    }
}
impl std::error::Error for Error {}
fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn hex(raw: &[u8], min: usize, max: usize) -> bool {
    (min..=max).contains(&raw.len())
        && raw
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}
fn checkpoint(interrupted: &AtomicBool, signal: Option<&AtomicI32>) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err(Error::new(process::interrupted_code(signal), "interrupted"))
    } else {
        Ok(())
    }
}
fn utc() -> DateTime<Utc> {
    SystemTime::now().into()
}
fn now() -> String {
    utc().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
static NEXT: AtomicU64 = AtomicU64::new(0);
fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "task-{}-{}-{}",
        utc().format("%Y%m%dT%H%M%SZ"),
        std::process::id(),
        (nanos + u128::from(NEXT.fetch_add(1, Ordering::Relaxed))) % 32768
    )
}
fn env(name: &str, default: &[u8]) -> Vec<u8> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map_or_else(|| default.to_vec(), |v| v.as_bytes().to_vec())
}
fn env_cap(name: &str, default: usize) -> usize {
    let v = env(name, default.to_string().as_bytes());
    if v.iter().all(u8::is_ascii_digit) {
        std::str::from_utf8(&v)
            .unwrap()
            .parse()
            .unwrap_or(usize::MAX)
    } else {
        default
    }
}
fn safe(raw: &[u8], kind: &str) -> Result<()> {
    // Canonical scanner is fail-closed; diagnostics contain no input bytes.
    let environment: Vec<_> = std::env::vars_os().collect();
    let entries: Vec<_> = environment
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect();
    match oms_scrub::scan(raw, &entries) {
        Ok(hits) if hits.is_empty() => Ok(()),
        _ => Err(Error::new(
            3,
            format!(
                "task {kind} contains sensitive-looking content; not {}",
                if kind == "note" {
                    "appended"
                } else {
                    "updated"
                }
            ),
        )),
    }
}
fn write(out: &mut dyn Write, raw: &[u8]) -> Result<()> {
    out.write_all(raw).map_err(|e| Error::io("write output", e))
}
fn line(out: &mut dyn Write, parts: &[&[u8]]) -> Result<()> {
    for part in parts {
        write(out, part)?;
    }
    write(out, b"\n")
}

/// This handler owns malformed and empty invocations as well as every mode.
pub fn owns(_: &[OsString]) -> bool {
    true
}
pub fn usage() -> &'static [u8] {
    cli::USAGE
}
/// Convenience buffered adapter. Read modes work without service injection;
/// mutations refuse without the real reference service. Use [`run_with_io`]
/// for streaming verification and explicit signal provenance.
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(args, default_repo, interrupted, &mut services::Unavailable)
}
pub fn run_with_services(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    services: &mut dyn Services,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_with_io(
        args,
        default_repo,
        interrupted,
        None,
        services,
        &mut std::io::stdin().lock(),
        &mut out,
        &mut err,
    );
    (code, out, err)
}
/// The stored Verify contract declares Bash, matching agent-task.sh's `bash -c`.
/// User commands inherit environment/stdin, execute in the selected repository,
/// and stream merged stdout/stderr. No stored command executes in other modes.
/// `signal` is the parent's observed HUP/INT/TERM number, never installed here.
/// No global environment, working directory, signal handler or Git ref changes.
#[allow(clippy::too_many_arguments)]
pub fn run_with_io(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    signal: Option<&AtomicI32>,
    services: &mut dyn Services,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    match execute(
        args,
        default_repo,
        interrupted,
        signal,
        services,
        input,
        out,
        err,
    ) {
        Ok(code) => code,
        Err(e) => {
            let _ = line(err, &[b"error: ", &e.message]);
            if e.message.starts_with(b"unknown command:")
                || e.message.starts_with(b"unknown argument:")
            {
                let _ = write(err, cli::USAGE);
            }
            e.code
        }
    }
}

struct Task<'a> {
    options: cli::Options,
    repo: PathBuf,
    state_repo: PathBuf,
    store: storage::Store,
    display_path: PathBuf,
    lock_path: PathBuf,
    agent: Vec<u8>,
    interrupted: &'a AtomicBool,
    signal: Option<&'a AtomicI32>,
    note_cap: usize,
    current_cap: usize,
}
impl Task<'_> {
    fn operation<T>(
        &self,
        services: &mut dyn Services,
        action: impl FnOnce(&mut dyn ReferenceOperation) -> Result<T>,
    ) -> Result<T> {
        let mut reference = services.begin_reference(&self.state_repo, &self.store.path)?;
        let result = storage::locked(&self.lock_path, self.interrupted, self.signal, || {
            action(reference.as_mut())
        });
        let finished = reference.finish(result.as_ref().err().map_or(0, |e| e.code));
        match (result, finished) {
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
            (Ok(v), Ok(())) => Ok(v),
        }
    }
    fn publish(
        &self,
        operation: &str,
        before: Option<&[u8]>,
        body: &[u8],
        reference: &mut dyn ReferenceOperation,
    ) -> Result<()> {
        self.store.write(
            &self.state_repo,
            operation,
            &self.options.arguments(),
            before,
            body,
            self.options.value("source-session"),
            reference,
            self.interrupted,
            self.signal,
        )
    }
    fn initialize(&self, existing: Option<Vec<u8>>) -> Vec<u8> {
        let stamp = now();
        let id = new_id();
        existing.map_or_else(
            || packet::initial(&stamp, &id, self.options.value("source-session")),
            |raw| packet::ensure(raw, &stamp, &id, self.options.value("source-session")),
        )
    }
    fn append(&self, raw: &[u8], section: &str, note: &[u8]) -> Vec<u8> {
        packet::append(
            raw,
            section,
            &self.agent,
            note,
            &now(),
            self.note_cap,
            self.current_cap,
        )
    }
    fn apply(&self, mut body: Vec<u8>) -> Result<Vec<u8>> {
        let o = &self.options;
        for key in [
            "goal",
            "verify",
            "last-failure",
            "verification",
            "state",
            "hypothesis",
            "result",
            "next",
            "constraint",
            "done",
            "decision",
            "verify-level",
        ] {
            if o.has(key) {
                safe(o.value(key), "section")?;
            }
        }
        for (key, name) in [("goal", "Goal"), ("verify", "Verify")] {
            if o.has(key) {
                body = packet::touch(
                    packet::replace(&body, name, &[o.value(key), b"\n"].concat()),
                    &now(),
                );
            }
        }
        if ["loop-attempts", "loop-max", "diff-budget", "verify-level"]
            .iter()
            .any(|k| o.has(k))
        {
            let mut content = Vec::new();
            for (option, key) in [
                ("loop-attempts", "attempts"),
                ("loop-max", "max_attempts"),
                ("diff-budget", "diff_budget_lines"),
                ("verify-level", "verification_level"),
            ] {
                let v = if o.has(option) {
                    o.value(option).to_vec()
                } else {
                    packet::section_value(&body, "Loop State", key)
                };
                if !v.is_empty() {
                    content.extend_from_slice(format!("- {key}: ").as_bytes());
                    content.extend_from_slice(&v);
                    content.push(b'\n');
                }
            }
            body = packet::touch(packet::replace(&body, "Loop State", &content), &now());
        }
        for (key, name) in [
            ("last-failure", "Last Failure"),
            ("verification", "Verification"),
        ] {
            if o.has(key) {
                body = self.append(&body, name, o.value(key));
            }
        }
        if o.has("state") {
            body = packet::touch(
                packet::replace(&body, "Current State", &[o.value("state"), b"\n"].concat()),
                &now(),
            );
        }
        for (key, prefix) in [
            ("hypothesis", b"Hypothesis: ".as_slice()),
            ("result", b"Result: ".as_slice()),
        ] {
            if o.has(key) {
                body = self.append(&body, "Current State", &[prefix, o.value(key)].concat());
            }
        }
        if o.has("next") {
            body = packet::touch(
                packet::replace(&body, "Next Step", &[o.value("next"), b"\n"].concat()),
                &now(),
            );
        }
        for (key, name) in [
            ("constraint", "Constraints"),
            ("done", "Done Criteria"),
            ("decision", "Decisions"),
        ] {
            if o.has(key) {
                body = self.append(&body, name, o.value(key));
            }
        }
        if o.has("source-session") {
            body = packet::touch(
                packet::set_metadata(&body, "source_session", o.value("source-session")),
                &now(),
            );
        }
        Ok(body)
    }
    /// Sets `github_issue` on a packet that is not being rotated away. The same
    /// ref (owner/repo compared case-insensitively) leaves the bytes untouched;
    /// a different ref needs `--replace-goal` and records the replaced ref.
    /// `issue` is the canonical `owner/repo#N` resolved against origin.
    fn bind(&self, body: Vec<u8>, issue: Option<&[u8]>) -> Result<Vec<u8>> {
        let Some(issue) = issue else {
            return Ok(body);
        };
        let bound = packet::metadata(&body, "github_issue")
            .unwrap_or_default()
            .to_vec();
        if bound.eq_ignore_ascii_case(issue) {
            return Ok(body);
        }
        let mut body = packet::touch(packet::set_metadata(&body, "github_issue", issue), &now());
        if !bound.is_empty() {
            if !self.options.has("replace-goal") {
                return Err(Error::bytes(
                    2,
                    [
                        b"refusing to rebind task issue ".as_slice(),
                        &bound,
                        b" to ",
                        issue,
                        b"\na parent can replace the binding with --replace-goal REASON",
                    ]
                    .concat(),
                ));
            }
            let note = [
                b"issue rebound (".as_slice(),
                &self.options.action,
                b"): previous github_issue=",
                &bound,
                b"; reason: ",
                self.options.value("replace-goal"),
            ]
            .concat();
            safe(&note, "note")?;
            body = self.append(&body, "Decisions", &note);
        }
        Ok(body)
    }
    fn replacement(&self, raw: &[u8]) -> Result<Vec<u8>> {
        let o = &self.options;
        if ![b"init".as_slice(), b"rotate", b"update"].contains(&o.action.as_slice())
            || raw.is_empty()
        {
            return Ok(Vec::new());
        }
        let old = packet::nonempty(&packet::section(raw, "Goal"));
        if old.is_empty() {
            return Ok(Vec::new());
        }
        if !o.mode("rotate")
            && (!o.has("goal") || old == packet::nonempty(&[o.value("goal"), b"\n"].concat()))
        {
            return Ok(Vec::new());
        }
        let id = packet::metadata(raw, "task_id").unwrap_or(b"legacy");
        if packet::metadata(raw, "status").unwrap_or(b"active") == b"active"
            && !packet::stale(raw, &env("OMS_AGENT_TASK_TTL", b"604800"), utc())
            && !o.has("replace-goal")
        {
            let mut message = [
                b"refusing to replace the goal of active task ".as_slice(),
                id,
                b": ",
                &packet::lines(&old).first().copied().unwrap_or_default()
                    [..packet::lines(&old).first().map_or(0, |l| l.len().min(80))],
            ]
            .concat();
            message.extend_from_slice(b"\nfinish it ('agent-task verify', then 'close'), or a parent can replace it with --replace-goal REASON");
            return Err(Error::bytes(2, message));
        }
        if !o.has("replace-goal") {
            return Ok(Vec::new());
        }
        let mut note = [
            b"goal replaced (".as_slice(),
            &o.action,
            b"): previous task_id=",
            id,
            b"; reason: ",
            o.value("replace-goal"),
        ]
        .concat();
        if o.mode("update") {
            let first = packet::lines(&old).first().copied().unwrap_or_default();
            note.extend_from_slice(b" (was: ");
            note.extend_from_slice(&packet::cap_utf8(first, 120));
            note.push(b')');
        }
        Ok(note)
    }
    fn archive(&self, body: Vec<u8>, reference: &mut dyn ReferenceOperation) -> Result<PathBuf> {
        let identity = packet::metadata(&body, "task_id")
            .ok_or_else(|| Error::new(1, "task has no archive identity"))?;
        if identity.is_empty() || identity.iter().any(|b| *b == b'/' || *b == 0) {
            return Err(Error::new(1, "unsafe task archive identity"));
        }
        let before = self
            .store
            .read()?
            .ok_or_else(|| Error::new(1, "task disappeared before archival"))?;
        let closed = packet::status(body, "closed", &now());
        self.publish(
            "agent_task_archive_unlocked",
            Some(&before),
            &closed,
            reference,
        )?;
        let id = packet::metadata(&closed, "task_id")
            .ok_or_else(|| Error::new(1, "task has no archive identity"))?;
        self.store.archive(
            &self.state_repo,
            &closed,
            id,
            &utc().format("%Y%m%dT%H%M%SZ").to_string(),
            &self.options.arguments(),
            self.options.value("source-session"),
            reference,
            self.interrupted,
            self.signal,
        )
    }
    fn journal(
        &self,
        services: &mut dyn Services,
        path: &Path,
        operation: JournalOperation,
        verification: Option<VerificationStatus>,
        err: &mut dyn Write,
    ) -> Result<()> {
        if !journal_enabled()
            || env("OMS_WORK_JOURNAL_ACTIVE", b"0") == b"1"
            || env("OMS_WORK_JOURNAL_SUPPRESS", b"0") == b"1"
        {
            return Ok(());
        }
        if services
            .observe_journal(JournalObservation {
                repo: &self.repo,
                path,
                operation,
                verification,
            })
            .is_err()
        {
            line(err,&[b"warning: Work Journal observer degraded; primary lifecycle result is unchanged"])?;
        }
        Ok(())
    }
    fn verification(&self, raw: &[u8]) -> Result<&'static str> {
        if packet::metadata(raw, "status") != Some(b"verified")
            || packet::metadata(raw, "verified_state").is_none_or(|v| v.is_empty())
        {
            return Ok("none");
        }
        let current = git::state_fingerprint(&self.repo, self.interrupted)?;
        if packet::metadata(raw, "verified_state") != Some(current.as_bytes()) {
            return Ok("stale");
        }
        if let Some(stored) = packet::metadata(raw, "verified_cmd_sha").filter(|v| !v.is_empty())
            && stored != git::command_hash(&packet::command(raw)).as_bytes()
        {
            return Ok("stale");
        }
        Ok("fresh")
    }
}
fn journal_enabled() -> bool {
    ![
        b"0".as_slice(),
        b"false",
        b"FALSE",
        b"no",
        b"NO",
        b"off",
        b"OFF",
    ]
    .contains(&env("OMS_WORK_JOURNAL", b"1").as_slice())
}

#[allow(clippy::too_many_arguments)]
fn execute(
    args: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    signal: Option<&AtomicI32>,
    services: &mut dyn Services,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let options = cli::parse(args)?;
    if options.help {
        write(out, cli::USAGE)?;
        return Ok(0);
    }
    cli::parent_guard(&options)?;
    checkpoint(interrupted, signal)?;
    if !cli::MODES.iter().any(|mode| options.mode(mode)) {
        return Err(Error::bytes(
            2,
            [b"unknown command: ".as_slice(), &options.action].concat(),
        ));
    }
    let selected = options.path("repo", cwd).unwrap_or_else(|| {
        std::env::var_os("OMS_STATE_REPO")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf())
    });
    if options.mode("path") && options.has("file") {
        line(out, &[options.value("file")])?;
        return Ok(0);
    }
    let repo = selected
        .canonicalize()
        .map_err(|e| Error::io("resolve execution repository", e))?;
    let work_root = if options.has("file") {
        repo.clone()
    } else {
        git::repository(&repo, interrupted)?
    };
    let path = options
        .path("file", cwd)
        .unwrap_or_else(|| work_root.join(".oms/task/current.md"));
    let display_path = if options.has("file") {
        PathBuf::from(OsStr::from_bytes(options.value("file")))
    } else {
        path.clone()
    };
    if options.mode("path") {
        line(out, &[path.as_os_str().as_bytes()])?;
        return Ok(0);
    }
    let store = storage::Store::new(&work_root, &path)?;
    if options.mode("verify") && store.read()?.is_none_or(|raw| raw.is_empty()) {
        return Err(Error::bytes(
            2,
            [
                b"no active task (".as_slice(),
                display_path.as_os_str().as_bytes(),
                b")",
            ]
            .concat(),
        ));
    }
    if options.mode("close") && store.read()?.is_none() {
        if options.has("if-unchanged-sha256") {
            return Err(Error::new(
                4,
                "task packet changed before close; archival skipped",
            ));
        }
        line(
            out,
            &[
                b"task: no active task (",
                display_path.as_os_str().as_bytes(),
                b")",
            ],
        )?;
        return Ok(0);
    }
    let agent = options.agent();
    let read_only = ["show", "context", "status"]
        .iter()
        .any(|mode| options.mode(mode));
    let issue = if options.values.contains_key("issue") {
        Some(resolve_issue(options.value("issue"), &repo, interrupted)?)
    } else {
        None
    };
    let state_repo = if read_only {
        repo.clone()
    } else {
        services
            .state_repository(&git::repository(&repo, interrupted)?, interrupted)?
            .canonicalize()
            .map_err(|e| Error::io("resolve state repository", e))?
    };
    let task = Task {
        options,
        repo,
        state_repo,
        store,
        display_path,
        lock_path: path.clone(),
        agent,
        interrupted,
        signal,
        note_cap: env_cap("OMS_AGENT_TASK_NOTE_CHARS", 300),
        current_cap: env_cap("OMS_AGENT_TASK_MAX_CURRENT_BULLETS", 100),
    };
    if task.options.mode("show") {
        match task.store.read()?.filter(|v| !v.is_empty()) {
            Some(raw) => write(out, &raw)?,
            None => line(
                out,
                &[
                    b"task: empty (",
                    task.display_path.as_os_str().as_bytes(),
                    b")",
                ],
            )?,
        }
        return Ok(0);
    }
    if task.options.mode("context") {
        context(&task, out, err)?;
        return Ok(0);
    }
    if task.options.mode("status") {
        status(&task, out)?;
        return Ok(0);
    }
    if task.options.mode("verify") {
        return verify(&task, services, out, err);
    }
    if task.options.mode("close") {
        return close(&task, services, out, err);
    }
    if task.options.mode("append") {
        let mut note = Vec::new();
        if task.options.stdin {
            input
                .take((storage::MAX_PACKET + 1) as u64)
                .read_to_end(&mut note)
                .map_err(|e| Error::io("read append input", e))?;
            if note.len() > storage::MAX_PACKET {
                return Err(Error::new(1, "task note exceeds read bound"));
            }
        } else if task.options.has("text") {
            note.extend_from_slice(task.options.value("text"));
            note.push(b'\n');
        } else {
            return Err(Error::new(2, "append requires --text or --stdin"));
        }
        safe(&note, "note")?;
        task.operation(services, |reference| {
            let before = task.store.read()?;
            let body = task.initialize(before.clone());
            if !task.options.has("dedupe-key")
                || !packet::receipt(&body, &note, task.options.value("dedupe-key"))?
            {
                let mut after = task.append(&body, "Current State", &note);
                if task.options.has("dedupe-key") {
                    after = packet::add_receipt(&after, &note, task.options.value("dedupe-key"))?;
                }
                if before.is_none() {
                    task.store.ensure_ignore();
                }
                task.publish(
                    if task.options.has("dedupe-key") {
                        "agent_task_append_deduplicated_unlocked"
                    } else {
                        "agent_task_append_bullet_unlocked"
                    },
                    before.as_deref(),
                    &after,
                    reference,
                )?;
            }
            Ok(())
        })?;
        line(
            out,
            &[b"task: appended ", task.display_path.as_os_str().as_bytes()],
        )?;
        return Ok(0);
    }
    let (archive, replacement, replacement_id) = task.operation(services, |reference| {
        let before = task.store.read()?;
        let replacement = task.replacement(before.as_deref().unwrap_or_default())?;
        let rotate = task.options.mode("rotate")
            || task.options.mode("init")
                && before.as_ref().is_some_and(|v| !v.is_empty())
                && task.options.has("goal");
        // Validate all supplied inputs before rotating the active file. A
        // rotated-away packet keeps its own binding, so only a surviving
        // packet is checked for an issue conflict.
        let mut candidate = task.apply(task.initialize(before.clone()))?;
        if !rotate {
            candidate = task.bind(candidate, issue.as_deref())?;
        }
        let archive = if rotate {
            let archived = if let Some(raw) = before.as_ref() {
                Some(task.archive(task.initialize(Some(raw.clone())), reference)?)
            } else {
                None
            };
            candidate = task.bind(task.apply(task.initialize(None))?, issue.as_deref())?;
            archived
        } else {
            None
        };
        if before.is_none() || rotate {
            task.store.ensure_ignore();
        }
        task.publish(
            "command_packet_locked",
            if rotate { None } else { before.as_deref() },
            &candidate,
            reference,
        )?;
        let replacement_id = packet::metadata(&candidate, "task_id")
            .unwrap_or_default()
            .to_vec();
        Ok((archive, replacement, replacement_id))
    })?;
    if let Some(archive) = archive {
        line(out, &[b"task: archived ", archive.as_os_str().as_bytes()])?;
    }
    if !replacement.is_empty() {
        safe(&replacement, "note")?;
        task.operation(services, |reference| {
            let raw = task.store.read()?.ok_or_else(|| {
                Error::new(
                    3,
                    "goal was replaced but the replacement note was not recorded",
                )
            })?;
            if packet::metadata(&raw, "task_id") != Some(replacement_id.as_slice()) {
                return Err(Error::new(
                    3,
                    "task rotated before its goal replacement note could be recorded",
                ));
            }
            task.publish(
                "agent_task_append_bullet_unlocked",
                Some(&raw),
                &task.append(&raw, "Decisions", &replacement),
                reference,
            )
        })?;
    }
    if task.options.journal_update() {
        task.journal(services, &path, JournalOperation::Update, None, err)?;
    }
    line(
        out,
        &[
            if task.options.mode("update") {
                b"task: updated "
            } else {
                b"task: initialized "
            },
            task.display_path.as_os_str().as_bytes(),
        ],
    )?;
    Ok(0)
}

/// Canonical `owner/repo#N` for `--issue`, resolved locally against `origin`
/// and any GitHub project declaration (PROJECT.md or the committed
/// `.github/oms-project.md`). Nothing contacts GitHub, and
/// diagnostics never echo the remote URL.
/// The project declaration is read from the toplevel of the repository whose
/// origin is used, even when `--file` places the packet elsewhere.
fn resolve_issue(raw: &[u8], repo: &Path, interrupted: &AtomicBool) -> Result<Vec<u8>> {
    let fail = |m: &str| Error::new(2, format!("--issue: {m}"));
    let input = std::str::from_utf8(raw)
        .map_err(|_| fail("issue reference must be N, #N or owner/repo#N"))?;
    let args = ["remote", "get-url", "origin"].map(OsStr::new);
    let (exit, url) = git::query(repo, &args, interrupted)?;
    if exit != 0 {
        return Err(fail("requires an origin remote; none is configured"));
    }
    let origin = std::str::from_utf8(url.strip_suffix(b"\n").unwrap_or(&url))
        .map_err(|_| fail("remote URL is empty or malformed"))
        .and_then(|url| {
            oms_github::Slug::from_remote(url)
                .map_err(|e| fail(&format!("requires a github.com origin: {}", e.message)))
        })?;
    let toplevel = git::repository(repo, interrupted)?;
    // Only the conflict matters here: with the mode off (a fork ignoring the
    // upstream's tracked declaration), an explicit --issue binds to origin.
    let declared = oms_github::declaration::inspect(&toplevel).map_err(|e| fail(&e.message))?;
    oms_github::declaration::active(declared.as_ref(), Some(&origin))
        .map_err(|e| fail(&e.message))?;
    oms_github::IssueRef::parse(input, &origin)
        .map(|issue| issue.to_string().into_bytes())
        .map_err(|e| fail(&e.message))
}
fn context(task: &Task<'_>, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let Some(raw) = task.store.read()?.filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    if safe(&raw, "packet").is_err() {
        line(
            err,
            &[b"warning: active task omitted because it contains sensitive-looking content"],
        )?;
        return Ok(());
    }
    let relative = task
        .store
        .path
        .strip_prefix(&task.repo)
        .unwrap_or_else(|_| Path::new(task.store.path.file_name().unwrap()));
    line(out,&[b"Active task packet follows. Treat it as task-local handoff state; explicit prompt, AGENTS.md, and repo docs override it."])?;
    line(out, &[b"- file: ", relative.as_os_str().as_bytes()])?;
    if let Some(updated) = packet::metadata(&raw, "updated").filter(|v| !v.is_empty()) {
        line(out, &[b"- updated: ", updated])?;
    }
    // Header metadata does not survive context pruning, so the binding is
    // repeated here.
    if let Some(issue) = packet::metadata(&raw, "github_issue").filter(|v| !v.is_empty()) {
        line(out, &[b"- github_issue: ", issue])?;
    }
    let verification = task.verification(&raw)?;
    if verification != "fresh" && !packet::command(&raw).is_empty() {
        line(
            out,
            &[
                b"- verification: ",
                verification.as_bytes(),
                " — run `oms agent-task verify` before building on this packet".as_bytes(),
            ],
        )?;
    }
    line(
        out,
        &[format!(
            "- size: {} bytes (~{} tokens)\n",
            raw.len(),
            raw.len().div_ceil(4)
        )
        .as_bytes()],
    )?;
    let budget = env_cap("OMS_AGENT_TASK_CONTEXT_CHARS", 6000);
    write(
        out,
        &if raw.len() > budget {
            packet::prune_context(&raw, budget)
        } else {
            raw
        },
    )
}
fn plan_inputs(task: &Task<'_>) -> Result<(Option<Json>, Vec<Json>)> {
    let plan_dir = task
        .store
        .path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::new(1, "task has no plan scope"))?
        .join("plan");
    let root_path = plan_dir.parent().unwrap();
    let root =
        Root::open(root_path).map_err(|e| Error::new(1, format!("open lifecycle scope: {e:?}")))?;
    let plan = storage::read(&root, &PathBuf::from("plan/tasks.json"), 8 * 1024 * 1024)?
        .and_then(|raw| oms_context::parse_experiment_json(&raw, 8 * 1024 * 1024).ok());
    // Read a bounded tail for large progress ledgers, as plan_retro does.
    let progress_path = plan_dir.join("progress.jsonl");
    let rows = projection::progress_tail(&progress_path)?;
    Ok((plan, rows))
}
fn status(task: &Task<'_>, out: &mut dyn Write) -> Result<()> {
    let Some(raw) = task.store.read()?.filter(|v| !v.is_empty()) else {
        return line(
            out,
            &[if task.options.json {
                b"{\"schema\": 1, \"present\": false}"
            } else {
                b"status: none"
            }],
        );
    };
    let id = packet::metadata(&raw, "task_id").unwrap_or(b"legacy");
    let state = packet::metadata(&raw, "status").unwrap_or(b"active");
    let source = packet::metadata(&raw, "source_session").unwrap_or_default();
    let issue = packet::metadata(&raw, "github_issue").unwrap_or_default();
    let activity = packet::metadata(&raw, "last_activity")
        .or_else(|| packet::metadata(&raw, "updated"))
        .unwrap_or_default();
    let closed = packet::metadata(&raw, "closed_at").unwrap_or_default();
    let stale = packet::stale(&raw, &env("OMS_AGENT_TASK_TTL", b"604800"), utc());
    let verification = task.verification(&raw)?;
    if !task.options.json {
        let mut bytes = Vec::new();
        for (k, v) in [
            ("task_id", id),
            ("status", state),
            ("verification", verification.as_bytes()),
            ("source_session", source),
            ("last_activity", activity),
            ("closed_at", closed),
            ("stale", if stale { b"yes" } else { b"no" }),
            ("github_issue", issue),
        ] {
            // Unbound packets keep the established text rows.
            if k == "github_issue" && v.is_empty() {
                continue;
            }
            bytes.extend_from_slice(k.as_bytes());
            bytes.extend_from_slice(b": ");
            bytes.extend_from_slice(v);
            bytes.push(b'\n');
        }
        return write(out, &bytes);
    }
    use json::{object, string};
    fn nullable(v: &[u8]) -> Json {
        if v.is_empty() { Json::Null } else { string(v) }
    }
    let lifecycle = if state == b"verified" && verification == "fresh"
        || ![b"active".as_slice(), b"verified"].contains(&state)
    {
        lifecycle::lifecycle(&raw, None, &[], verification)
    } else {
        match plan_inputs(task) {
            Ok((plan, rows)) => lifecycle::lifecycle(&raw, plan.as_ref(), &rows, verification),
            Err(_) => object([
                ("state", string("unknown")),
                (
                    "evidence",
                    object([("rule", string("lifecycle-unavailable"))]),
                ),
            ]),
        }
    };
    // An unbound packet omits `github_issue`, keeping the established bytes.
    let value = object(
        [
            ("schema", Json::Number("1".into())),
            ("present", Json::Bool(true)),
            ("task_id", string(id)),
            ("status", string(state)),
            ("source_session", nullable(source)),
            ("github_issue", nullable(issue)),
            ("last_activity", nullable(activity)),
            ("closed_at", nullable(closed)),
            ("stale", Json::Bool(stale)),
            ("verification", string(verification)),
            ("lifecycle", lifecycle),
        ]
        .into_iter()
        .filter(|(k, _)| *k != "github_issue" || !issue.is_empty()),
    );
    let encoded = json::encode(&value);
    oms_work_inspect::task_status_snapshot(&encoded).map_err(|e| Error::new(1, e.to_string()))?;
    line(out, &[&encoded])
}

fn verify(
    task: &Task<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let (id, command) = task.operation(services, |reference| {
        let raw = task
            .store
            .read()?
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::new(2, "no active task"))?;
        let body = packet::status(
            task.apply(task.initialize(Some(raw.clone())))?,
            "active",
            &now(),
        );
        let id = packet::metadata(&body, "task_id")
            .unwrap_or_default()
            .to_vec();
        let command = packet::command(&body);
        task.publish(
            "agent_task_verify_snapshot_unlocked",
            Some(&raw),
            &body,
            reference,
        )?;
        Ok((id, command))
    })?;
    if task.options.has("skip-verify-run") {
        let note = [
            b"command: ".as_slice(),
            if command.is_empty() {
                b"(none)"
            } else {
                &command
            },
            b"\nresult: SKIPPED\nreason: ",
            task.options.value("skip-verify-run"),
            b"\nrecorded_at: ",
            now().as_bytes(),
            b"\n",
        ]
        .concat();
        finalize(task, services, &id, &note, None)?;
        task.journal(
            services,
            &task.store.path,
            JournalOperation::Verify,
            None,
            err,
        )?;
        line(
            out,
            &[
                b"task: verification skipped; task remains active (",
                task.display_path.as_os_str().as_bytes(),
                b")",
            ],
        )?;
        return Ok(0);
    }
    if command.is_empty() {
        return Err(Error::new(
            2,
            "task has no stored Verify command; set one with --verify or use --skip-verify-run REASON",
        ));
    }
    let start = utc();
    let mut process = Command::new("bash");
    process
        .arg("-c")
        .arg(OsStr::from_bytes(&command))
        .current_dir(&task.repo);
    let result = process::execute(
        &mut process,
        true,
        true,
        out,
        16 * 1024 * 1024,
        task.interrupted,
        task.signal,
    )?;
    let finish = utc();
    let observation = services
        .observe_verify(&task.repo, &result.tail)
        .unwrap_or_default();
    let mut note = [
        b"command: ".as_slice(),
        &command,
        format!(
            "\nexit: {}\nstarted_at: {}\nfinished_at: {}\nduration_seconds: {}\n",
            result.exit,
            start.format("%Y-%m-%dT%H:%M:%SZ"),
            finish.format("%Y-%m-%dT%H:%M:%SZ"),
            finish.timestamp() - start.timestamp()
        )
        .as_bytes(),
    ]
    .concat();
    if !observation.lines.is_empty() {
        note.extend_from_slice(&observation.lines);
        note.push(b'\n');
    }
    // Preserve the existing two receipt legs: the outcome is first recorded
    // active; only the successful CAS adds fresh state and command receipts.
    finalize(task, services, &id, &note, None)?;
    if result.exit == 0 {
        let fingerprint = git::state_fingerprint(&task.repo, task.interrupted)?;
        finalize(
            task,
            services,
            &id,
            &note,
            Some((&fingerprint, &git::command_hash(&command))),
        )?;
    }
    // Resolve advisory failures only after the successful receipt is accepted.
    let summary = failure_summary(&result.tail);
    match services.observe_failure(FailureObservation {
        repo: &task.repo,
        command: &command,
        exit: result.exit,
        summary: &summary,
    }) {
        Ok(advisory) => {
            if !advisory.is_empty() {
                write(err, &advisory)?;
            }
        }
        Err(_) => {
            if result.exit != 0 {
                line(
                    err,
                    &[b"note: verification failure was not recorded in the fail-ledger"],
                )?;
            }
        }
    }
    if result.exit != 0 {
        task.journal(
            services,
            &task.store.path,
            JournalOperation::Verify,
            None,
            err,
        )?;
        line(
            err,
            &[
                format!(
                    "task: verification failed (exit {}); task remains active (",
                    result.exit
                )
                .as_bytes(),
                task.display_path.as_os_str().as_bytes(),
                b")",
            ],
        )?;
        return Ok(result.exit);
    }
    task.journal(
        services,
        &task.store.path,
        JournalOperation::Verify,
        None,
        err,
    )?;
    line(
        out,
        &[b"task: verified ", task.display_path.as_os_str().as_bytes()],
    )?;
    if !observation.zero_runners.is_empty() {
        line(out,&[format!("task: warning: exit 0 but {} reported no tests run (advisory; the verify status is unchanged)",observation.zero_runners.join(", ")).as_bytes()])?;
    }
    Ok(0)
}
fn finalize(
    task: &Task<'_>,
    services: &mut dyn Services,
    id: &[u8],
    note: &[u8],
    receipts: Option<(&str, &str)>,
) -> Result<()> {
    task.operation(services,|reference|{
        let raw=task.store.read()?.ok_or_else(||Error::new(4,"task rotated during verification; outcome was not recorded"))?;
        if packet::metadata(&raw,"task_id")!=Some(id){return Err(Error::new(4,"task rotated during verification; outcome belongs to the retired packet and was not recorded"));}
        let mut body=task.append(&raw,"Verification",note);
        if let Some((state,command))=receipts {
            // A simultaneous edit of Verify cannot inherit the old command's
            // pass, even when its task identity remained unchanged.
            if git::command_hash(&packet::command(&raw))!=command {return Err(Error::new(4,"Verify contract changed during verification; pass was not recorded"));}
            if git::state_fingerprint(&task.repo,task.interrupted)?!=state{return Err(Error::new(4,"repository changed before verification receipt; pass was not recorded"));}
            body=packet::set_metadata(&body,"verified_state",state.as_bytes());body=packet::set_metadata(&body,"verified_cmd_sha",command.as_bytes());body=packet::status(body,"verified",&now());
        }
        task.publish("agent_task_verify_finalize_unlocked",Some(&raw),&body,reference)
    })
}
fn failure_summary(raw: &[u8]) -> Vec<u8> {
    let rows = packet::lines(raw);
    let first = rows
        .iter()
        .copied()
        .find(|l| {
            [b"FAIL".as_slice(), b"ERROR", b"error:", b"fatal:"]
                .iter()
                .any(|prefix| l.starts_with(prefix))
        })
        .or_else(|| {
            rows.iter()
                .rev()
                .copied()
                .find(|l| !packet::trim(l).is_empty())
        })
        .unwrap_or_default();
    packet::cap_utf8(first, 200)
}
fn close(
    task: &Task<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    // All close decisions are made under the same packet lock as archive. The
    // memory producer runs before archival, preserving the legacy order.
    let mut reference = services.begin_reference(&task.state_repo, &task.store.path)?;
    let result = storage::locked(&task.lock_path, task.interrupted, task.signal, || {
        let Some(raw) = task.store.read()? else {
            if task.options.has("if-unchanged-sha256") {
                return Err(Error::new(
                    4,
                    "task packet changed before close; archival skipped",
                ));
            }
            line(
                out,
                &[
                    b"task: no active task (",
                    task.display_path.as_os_str().as_bytes(),
                    b")",
                ],
            )?;
            return Ok(None);
        };
        if task.options.has("if-unchanged-sha256")
            && hash(&raw).as_bytes() != task.options.value("if-unchanged-sha256")
        {
            return Err(Error::new(
                4,
                "task packet changed before close; archival skipped",
            ));
        }
        reference.validate(&ReferenceWrite {
            repo: &task.state_repo,
            target: &task.store.path,
            operation: "agent_task_archive_unlocked",
            arguments: &task.options.arguments(),
            before: &raw,
            proposed: &raw,
            source_session: task.options.value("source-session"),
        })?;
        let verification = task.verification(&raw)?;
        let command = packet::command(&raw);
        if verification != "fresh" && !command.is_empty() && !task.options.has("reason") {
            return Err(Error::new(
                2,
                if verification == "stale" {
                    "verification is stale (the repo changed since it passed)\nrun 'agent-task.sh verify', or close anyway with --reason TEXT"
                } else {
                    "task is not verified\nrun 'agent-task.sh verify', or close anyway with --reason TEXT"
                },
            ));
        }
        let mut body = task.initialize(Some(raw.clone()));
        if task.options.has("reason") && verification != "fresh" {
            let note = [
                format!("closed without fresh verification ({verification}): ").as_bytes(),
                task.options.value("reason"),
                b"\n",
            ]
            .concat();
            safe(&note, "note")?;
            body = task.append(&body, "Verification", &note);
            task.publish(
                "agent_task_append_bullet_unlocked",
                Some(&raw),
                &body,
                reference.as_mut(),
            )?;
        }
        fn selected(raw: &[u8], name: &str, last: bool) -> Vec<u8> {
            let section = packet::section(raw, name);
            let values: Vec<_> = packet::lines(&section)
                .into_iter()
                .filter(|l| !packet::trim(l).is_empty())
                .collect();
            if last { values.last() } else { values.first() }
                .copied()
                .unwrap_or_default()
                .to_vec()
        }
        let goal = selected(&body, "Goal", false);
        let next = selected(&body, "Next Step", false);
        let decision = selected(&body, "Decisions", true);
        let pitfall = selected(&body, "Last Failure", true);
        if journal_enabled() && decision.is_empty() && next.is_empty() {
            line(err,&[b"hint: no decision or next step recorded; 'agent-task update --decision/--next' before close feeds the Work Journal daily"])?;
        }
        if env("OMS_SKILL_FORGE_HINT", b"1") == b"1"
            && task.repo.join(".oms/skills").is_dir()
            && let Ok(contracts) = services.skill_contracts(&task.repo)
        {
            for (name, command) in contracts {
                line(
                    err,
                    &[
                        b"hint: project skill '",
                        &name,
                        b"' declares verify contract: ",
                        &command,
                    ],
                )?;
            }
        }
        if env("OMS_AGENT_TASK_CLOSE_MEMORY", b"0") == b"1" && !goal.is_empty() {
            let mut outcome = [b"Closed task: ".as_slice(), &goal].concat();
            if !next.is_empty() {
                outcome.extend_from_slice(b" | next: ");
                outcome.extend_from_slice(&next);
            }
            outcome.push(b'\n');
            for (prefix, value) in [
                (b"decision: ".as_slice(), &decision),
                (b"pitfall: ".as_slice(), &pitfall),
            ] {
                if !value.is_empty() {
                    outcome.extend_from_slice(prefix);
                    outcome.extend_from_slice(value.strip_prefix(b"- ").unwrap_or(value));
                    outcome.push(b'\n');
                }
            }
            if services
                .promote_memory(MemoryOutcome {
                    repo: &task.repo,
                    agent: &task.agent,
                    text: &outcome,
                    source: "task_close",
                    task_id: packet::metadata(&body, "task_id").unwrap_or_default(),
                    verification: if verification == "fresh" {
                        VerificationStatus::Passed
                    } else {
                        VerificationStatus::NotVerified
                    },
                })
                .is_ok()
            {
                line(out, &[b"task: outcome queued for project memory review"])?;
            } else {
                line(err, &[b"warning: task outcome not added to shared memory"])?;
            }
        }
        // Services can run long; compare the exact task and fresh Git boundary
        // again before writing closed state, never archiving a successor.
        let expected = if body != raw && task.options.has("reason") && verification != "fresh" {
            body.as_slice()
        } else {
            raw.as_slice()
        };
        if task.store.read()?.as_deref() != Some(expected) {
            return Err(Error::new(
                4,
                "task changed during close; archive not published",
            ));
        }
        if verification == "fresh" && task.verification(expected)? != "fresh" {
            return Err(Error::new(
                2,
                "verification became stale during close; archive not published",
            ));
        }
        let archive = task.archive(body, reference.as_mut())?;
        Ok(Some((
            archive,
            if verification == "fresh" {
                VerificationStatus::Passed
            } else {
                VerificationStatus::NotVerified
            },
        )))
    });
    let finished = reference.finish(result.as_ref().err().map_or(0, |e| e.code));
    let archived = result?;
    finished?;
    if let Some((archive, verification)) = archived {
        task.journal(
            services,
            &archive,
            JournalOperation::Close,
            Some(verification),
            err,
        )?;
        line(out, &[b"task: archived ", archive.as_os_str().as_bytes()])?;
    }
    Ok(0)
}
