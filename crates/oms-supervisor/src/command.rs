//! Thin command adapter over canonical lifecycle operations and owned processes.
use crate::Result;
use crate::cli::{self, Args, Parsed};
use crate::lifecycle as life;
use crate::process::{Completion, JobId, Progress, Reactor, Request, Scope, StopReason};
use crate::storage::{self, Store};
use oms_command_operations::{
    self as ops,
    lock::FileLock,
    value::{V, obj},
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACTIVE: &[&str] = &[
    "launching",
    "starting",
    "running",
    "cancel_requested",
    "cleanup_failed",
];
const DEFAULT_LOG: usize = 8 * 1024 * 1024;
/// Retain this host across library calls. It owns runner and timed-out helper
/// handles until drive() observes their reap. The standalone CLI exits only
/// after recording runner identity, transferring those runners to the OS reaper.
pub struct CommandHost {
    pub runner_argv: Vec<OsString>,
    reactor: Reactor,
    retained: Vec<JobId>,
    detached: Vec<JobId>,
    helpers: BTreeSet<JobId>,
    trees: Vec<(Reactor, JobId)>,
    pub received_signal: Option<fn() -> Option<i32>>,
    foreground_cleanup: bool,
}
impl CommandHost {
    pub fn new(runner_argv: Vec<OsString>) -> Self {
        Self {
            runner_argv,
            reactor: Reactor::new(),
            retained: vec![],
            detached: vec![],
            helpers: BTreeSet::new(),
            trees: vec![],
            received_signal: None,
            foreground_cleanup: false,
        }
    }
    /// A CLI must keep driving if pending child identity could not be persisted.
    pub fn requires_foreground_cleanup(&self) -> bool {
        !self.helpers.is_empty() || self.foreground_cleanup && !self.trees.is_empty()
    }
    pub fn pending(&self) -> usize {
        self.reactor.pending() + self.trees.iter().map(|(r, _)| r.pending()).sum::<usize>()
    }
    pub fn drive(&mut self) -> Result<Vec<Completion>> {
        let mut done = Vec::new();
        let mut pending = Vec::new();
        let mut failure = None;
        for id in std::mem::take(&mut self.retained) {
            match self.reactor.poll(id, Duration::ZERO) {
                Ok(Progress::Complete(result)) => {
                    self.helpers.remove(&id);
                    self.detached.retain(|existing| *existing != id);
                    done.push(result);
                }
                Ok(Progress::Pending(_)) => pending.push(id),
                Err(error) => {
                    pending.push(id);
                    failure.get_or_insert(error);
                }
            }
        }
        self.retained = pending;
        let mut trees = Vec::new();
        for (mut reactor, id) in std::mem::take(&mut self.trees) {
            match reactor.poll(id, Duration::ZERO) {
                Ok(Progress::Complete(result)) => done.push(result),
                Ok(Progress::Pending(_)) => trees.push((reactor, id)),
                Err(error) => {
                    trees.push((reactor, id));
                    failure.get_or_insert(error);
                }
            }
        }
        self.trees = trees;
        match failure {
            Some(error) => Err(error),
            None => Ok(done),
        }
    }
    fn query(
        &mut self,
        argv: Vec<OsString>,
        cwd: Option<PathBuf>,
        interrupted: &AtomicBool,
    ) -> Result<Vec<u8>> {
        let mut request = Request::new(argv);
        request.cwd = cwd;
        request.scope = Scope::Leader;
        request.deadline = Instant::now().checked_add(Duration::from_secs(3));
        request.capture.max_bytes = 1024 * 1024;
        request.capture.on_limit = crate::capture::LimitAction::Terminate;
        let id = self.reactor.start(request)?;
        self.retained.push(id);
        self.helpers.insert(id);
        let until = Instant::now() + Duration::from_secs(8);
        loop {
            if interrupted.load(Ordering::Relaxed) {
                self.reactor.stop(id, StopReason::SupervisorError)?;
            }
            match self.reactor.poll(id, Duration::from_millis(50))? {
                Progress::Complete(result) => {
                    self.retained.retain(|existing| *existing != id);
                    self.helpers.remove(&id);
                    if result.stop.is_some()
                        || !result.cleanup_complete()
                        || !result.output_complete()
                        || result.exit.is_none_or(|e| e.shell_code() != 0)
                    {
                        return Err("native helper command failed or cleanup is incomplete".into());
                    }
                    return Ok(result.stdout.bytes);
                }
                Progress::Pending(_) if Instant::now() >= until => {
                    return Err("native helper cleanup pending in command host".into());
                }
                Progress::Pending(_) => {}
            }
        }
    }
    fn git_common(&mut self, path: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
        let raw = self.query(
            vec![
                "git".into(),
                "-C".into(),
                path.as_os_str().into(),
                "rev-parse".into(),
                "--git-common-dir".into(),
            ],
            None,
            interrupted,
        )?;
        let raw = trim_line(&raw);
        if raw.is_empty() {
            return Err("Git common directory unavailable".into());
        }
        let p = Path::new(OsStr::from_bytes(raw));
        std::fs::canonicalize(if p.is_absolute() {
            p.to_path_buf()
        } else {
            path.join(p)
        })
        .map_err(|e| e.to_string())
    }
    fn repo(&mut self, requested: &Path, cwd: &Path, interrupted: &AtomicBool) -> Result<PathBuf> {
        let requested = storage::expand(requested)?;
        let path = if requested.is_absolute() {
            requested
        } else {
            cwd.join(requested)
        };
        let queried = self.query(
            vec![
                "git".into(),
                "-C".into(),
                path.as_os_str().into(),
                "rev-parse".into(),
                "--show-toplevel".into(),
            ],
            None,
            interrupted,
        );
        let path = match queried {
            Ok(raw) if !trim_line(&raw).is_empty() => {
                PathBuf::from(OsStr::from_bytes(trim_line(&raw)))
            }
            _ => path,
        };
        std::fs::canonicalize(path).map_err(|_| "repository path does not exist".into())
    }
}
impl Drop for CommandHost {
    fn drop(&mut self) {
        let until = Instant::now() + crate::process::MAX_DROP_WAIT;
        while self.pending() != 0 && Instant::now() < until {
            let _ = self.drive();
            std::thread::sleep(Duration::from_millis(50));
        }
        // Persisted runners own their trees independently. Do not kill their
        // protocol owner when the dispatch host is dropped. They are handed
        // to the OS reaper when this process exits; retain this host to reap
        // them within a long-lived library caller.
        self.reactor.release_detached(&self.detached);
    }
}

fn trim_line(raw: &[u8]) -> &[u8] {
    let mut end = raw.len();
    while end > 0 && matches!(raw[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &raw[..end]
}
pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    host: &mut CommandHost,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let exit = run_streaming(args, default_repo, interrupted, host, &mut out, &mut err);
    (exit, out, err)
}
pub fn run_streaming(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
    host: &mut CommandHost,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let parsed = match cli::parse(args) {
        Ok(Parsed::Help(help)) => {
            return if out.write_all(help.as_bytes()).is_ok() {
                0
            } else {
                2
            };
        }
        Ok(Parsed::Args(args)) => args,
        Err(e) => {
            let _ = writeln!(err, "{}agent-supervisor.sh: error: {e}", cli::help(None));
            return 2;
        }
    };
    let result = (|| {
        child_guard(&parsed.command, parsed.flag("apply"))?;
        check(interrupted)?;
        let default_repo = std::fs::canonicalize(default_repo).map_err(|e| e.to_string())?;
        let repo = host.repo(&parsed.repo, &default_repo, interrupted)?;
        let store = Store::open(&repo, &default_repo)?;
        match parsed.command.as_str() {
            "submit" => {
                let id = submit(&repo, &default_repo, &store, &parsed, host, interrupted)?;
                line(out, id)?;
                Ok(0)
            }
            "dispatch" => {
                dispatch(&repo, &store, &parsed, host, interrupted, out)?;
                Ok(0)
            }
            "status" => {
                let row = public(
                    &store.load(&parsed.text("attempt")?)?,
                    &life::projection(&repo)?,
                    host,
                    interrupted,
                )?;
                if parsed.flag("json") {
                    json(out, &row)?;
                } else {
                    line(out, format!("attempt: {}", text(&row, "attempt_id")))?;
                    line(
                        out,
                        format!(
                            "lifecycle/runtime: {}/{}",
                            text(&row, "state"),
                            text(&row, "runtime_state")
                        ),
                    )?;
                    line(
                        out,
                        format!(
                            "provider/profile: {}/{}",
                            text(&row, "provider"),
                            text(&row, "profile")
                        ),
                    )?;
                    line(
                        out,
                        format!(
                            "exit: {}",
                            if row.get("exit_code") == &V::Null {
                                "-".into()
                            } else {
                                row.get("exit_code").py()
                            }
                        ),
                    )?;
                }
                Ok(0)
            }
            "wait" => wait(&repo, &store, &parsed, host, interrupted, out),
            "attach" => {
                attach(&store, &parsed, out)?;
                Ok(0)
            }
            "cancel" => {
                let row = cancel(&repo, &store, &parsed, host, interrupted)?;
                if parsed.flag("json") {
                    json_line(out, &row)?;
                } else {
                    line(out, text(&row, "attempt"))?;
                }
                Ok(0)
            }
            "resume" => {
                let id = resume(&repo, &store, &parsed, host, interrupted)?;
                line(out, id)?;
                Ok(0)
            }
            "reconcile" => {
                reconcile(&repo, &store, &parsed, host, interrupted, out)?;
                Ok(0)
            }
            "gc" => {
                gc(&repo, &store, &parsed, host, interrupted, out)?;
                Ok(0)
            }
            "serve" => {
                serve(&repo, &store, &parsed, host, interrupted, out)?;
                Ok(0)
            }
            "_run-one" => runner(
                &repo,
                &store,
                &parsed.text("attempt")?,
                host,
                interrupted,
                err,
            ),
            _ => unreachable!(),
        }
    })();
    match result {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            2
        }
    }
}
fn child_guard(command: &str, apply: bool) -> Result<()> {
    let child = std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|s| s == "1")
        || std::env::var_os("OMS_HARNESS_DELEGATE_DEPTH")
            .is_some_and(|s| s != "0" && !s.is_empty());
    if child
        && !(matches!(command, "status" | "wait" | "attach")
            || matches!(command, "reconcile" | "gc") && !apply)
    {
        Err("a harness child may only use read-only runtime actions".into())
    } else {
        Ok(())
    }
}
fn check(interrupted: &AtomicBool) -> Result<()> {
    if interrupted.load(Ordering::Relaxed) {
        Err("operation interrupted".into())
    } else {
        Ok(())
    }
}
fn text<'a>(row: &'a V, key: &str) -> &'a str {
    row.get(key).text().unwrap_or("")
}
fn integer(row: &V, key: &str) -> i64 {
    row.get(key).py().parse().unwrap_or(0)
}
fn line(out: &mut dyn Write, s: impl std::fmt::Display) -> Result<()> {
    writeln!(out, "{s}").map_err(|e| e.to_string())
}
fn json(out: &mut dyn Write, row: &V) -> Result<()> {
    out.write_all(&row.json(true)?).map_err(|e| e.to_string())
}
fn json_line(out: &mut dyn Write, row: &V) -> Result<()> {
    let mut raw = row.json(false)?;
    raw.push(b'\n');
    out.write_all(&raw).map_err(|e| e.to_string())
}
fn finish(job: &mut V, state: &str, exit: V, settlement: (&str, &str, &str)) {
    job.set("runtime_state", state);
    job.set("exit_code", exit);
    for key in ["pid", "runner_pid"] {
        job.set(key, 0usize);
    }
    for key in ["pid_identity", "runner_pid_identity"] {
        job.set(key, "");
    }
    job.set("finished_at", storage::now());
    let (target, reason, key) = settlement;
    let mut facts = obj([
        ("schema", 1usize.into()),
        ("state", target.into()),
        ("reason_code", reason.into()),
        ("idempotency_key", key.into()),
    ]);
    if matches!(target, "review" | "done") {
        // Written only after owned cleanup/output and the completion contract
        // have been checked; an exit code alone never creates this proof.
        facts.set("command_sha256", job.get("command_sha256").clone());
        facts.set("completion_state", job.get("completion_state").clone());
    }
    job.set("finish", facts);
}

fn finish_facts(job: &V) -> Result<V> {
    let facts = job.get("finish");
    if facts == &V::Null {
        // Older records cannot distinguish successful output/cleanup from an
        // interrupted runner that happened to observe exit zero.
        let (state, reason) = match text(job, "runtime_state") {
            "cancelled" => ("cancelled", "cancelled_by_owner"),
            "finished" if optional_integer(job, "exit_code")?.is_some_and(|n| n != 0) => {
                ("failed", "terminal_exit_nonzero")
            }
            _ => ("blocked", "terminal_evidence_missing"),
        };
        return Ok(obj([
            ("state", state.into()),
            ("reason_code", reason.into()),
            ("idempotency_key", "reconcile-legacy-terminal".into()),
        ]));
    }
    if facts.get("schema") != &V::from(1usize)
        || ![
            "blocked",
            "failed",
            "cancelled",
            "timed_out",
            "review",
            "done",
        ]
        .contains(&text(facts, "state"))
        || facts.get("reason_code").text().is_none()
        || !ops::projection::id(text(facts, "idempotency_key"))
    {
        return Err("invalid supervised finish facts".into());
    }
    Ok(facts.clone())
}

fn settlement_pending(job: &V, attempt: &V) -> Result<bool> {
    let current = text(attempt, "state");
    if text(attempt, "tool") != "agent-supervisor" || !life::nonterminal(current) {
        return Ok(false);
    }
    if job.get("finish") == &V::Null
        && current == "review"
        && text(job, "completion_state") != "done"
        && job.get("exit_code") == &V::from(0i64)
    {
        // An existing canonical review already settles a legacy review
        // contract; replay need not infer new success from its exit status.
        return Ok(false);
    }
    let facts = finish_facts(job)?;
    let target = text(&facts, "state");
    // A later block is not authority to reopen work.
    Ok(current != target && (!matches!(target, "review" | "done") || current != "blocked"))
}

fn settle_finish(repo: &Path, job: &V, interrupted: &AtomicBool) -> Result<()> {
    let id = text(job, "attempt_id");
    let facts = finish_facts(job)?;
    let target = text(&facts, "state");
    let mut current = life::projection(repo)?.get(id).cloned().unwrap_or(V::Null);
    if !settlement_pending(job, &current)? {
        return Ok(());
    }
    if matches!(target, "review" | "done") {
        if !matches!(text(&current, "state"), "working" | "verifying" | "review") {
            return life::transition(
                repo,
                id,
                "blocked",
                "terminal_settlement_conflict",
                "reconcile-terminal-conflict",
                interrupted,
            );
        }
        let specification = spec(repo, id)?;
        if text(job, "runtime_state") != "finished"
            || job.get("exit_code") != &V::from(0i64)
            || text(&facts, "command_sha256").is_empty()
            || facts.get("command_sha256") != specification.get("command_sha256")
            || facts.get("command_sha256") != job.get("command_sha256")
            || facts.get("command_sha256") != current.get("refs").get("command_sha256")
            || facts.get("completion_state") != specification.get("completion_state")
            || facts.get("completion_state") != job.get("completion_state")
            || facts.get("completion_state").text() != Some(target)
            || text(&specification, "profile") != "trusted-local"
        {
            return Err(
                "supervised finish proof does not match immutable completion intent".into(),
            );
        }
        if hard_budget(&current) {
            return life::transition(
                repo,
                id,
                "failed",
                "hard_budget_unsupported",
                "runner-hard-budget-unsupported",
                interrupted,
            );
        }
        if text(&current, "state") == "working" {
            life::transition(repo, id, "verifying", "", "runner-verifying", interrupted)?;
            current = life::projection(repo)?.get(id).cloned().unwrap_or(V::Null);
        }
        if text(&current, "state") == "verifying" {
            life::transition(repo, id, "review", "", "runner-review", interrupted)?;
        }
        if target == "review" {
            return Ok(());
        }
    }
    life::transition(
        repo,
        id,
        target,
        text(&facts, "reason_code"),
        text(&facts, "idempotency_key"),
        interrupted,
    )
}
fn command(argv: &[OsString]) -> Result<(Vec<OsString>, V, String)> {
    let argv = if argv.first().is_some_and(|s| s == "--") {
        &argv[1..]
    } else {
        argv
    };
    if argv.first().is_none_or(|s| s.is_empty()) {
        return Err("a command is required after --".into());
    }
    let strings: Vec<_> = argv
        .iter()
        .map(|s| {
            s.to_str()
                .ok_or("command cannot be encoded as durable UTF-8")
        })
        .collect::<std::result::Result<_, _>>()?;
    if strings.len() > 256 || strings.iter().map(|s| s.chars().count()).sum::<usize>() > 32768 {
        return Err("command is too large for a durable supervised job".into());
    }
    let pattern = concat!(
        r"(?i)(api[_-]?key|access[_-]?",
        "to",
        r"ken|sec",
        r"ret|pass",
        r"word|private[_-]?key)\s*[:",
        r"=]|AKIA[0-9A-Z]{16}|-----BE",
        "GIN [A-Z ]+PRIVATE ",
        "KEY-----"
    );
    let sensitive = regex::Regex::new(pattern).map_err(|e| e.to_string())?;
    if strings
        .iter()
        .any(|s| s.contains('\0') || sensitive.is_match(s))
    {
        return Err("command contains sensitive-looking material; pass credentials through a scoped runtime instead".into());
    }
    let values = V::Arr(strings.into_iter().map(V::from).collect());
    let digest = storage::digest(&values.json(false)?);
    Ok((argv.to_vec(), values, digest))
}
fn spec(repo: &Path, id: &str) -> Result<V> {
    storage::valid_id(id)?;
    V::parse(&ops::storage::read_bytes(&ops::attempt_spec_path(
        repo, id,
    ))?)
}
fn save_new(repo: &Path, store: &Store, job: &mut V, interrupted: &AtomicBool) -> Result<()> {
    let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
    store.root.check()?;
    let _events = FileLock::acquire(&ops::event_path(repo), interrupted)?;
    let attempts = life::projection(repo)?;
    let id = text(job, "attempt_id").to_owned();
    let attempt = attempts
        .get(&id)
        .ok_or("runtime job is not owned by an agent-supervisor attempt")?;
    if text(attempt, "tool") != "agent-supervisor" {
        return Err("runtime job is not owned by an agent-supervisor attempt".into());
    }
    if text(attempt, "state") != "queued" {
        return Err(format!(
            "runtime job cannot publish after lifecycle state {}",
            text(attempt, "state")
        ));
    }
    let semantics = |mut row: V| {
        if let V::Obj(ref mut m) = row {
            m.remove(b"updated_at".as_slice());
        }
        row
    };
    if store.exists(&id)? {
        if semantics(store.load(&id)?) != semantics(job.clone()) {
            return Err("runtime job already exists with different supervisor intent".into());
        }
    } else {
        store.save(job)?;
    }
    // Reconfirm both durability and contents on idempotent recovery after an
    // earlier rename whose directory sync may have failed.
    store.sync_jobs()?;
    if semantics(store.load(&id)?) != semantics(job.clone()) {
        return Err("runtime job changed after publication".into());
    }
    Ok(())
}
fn submit(
    repo: &Path,
    default_repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<String> {
    let (_, argv, digest) = command(&args.argv)?;
    let cwd = match args.raw("cwd").filter(|s| !s.is_empty()) {
        Some(raw) => {
            let path = storage::expand(Path::new(raw))?;
            std::fs::canonicalize(if path.is_absolute() {
                path
            } else {
                default_repo.join(path)
            })
            .map_err(|e| e.to_string())?
        }
        None => repo.to_owned(),
    };
    if !cwd.is_dir() || host.git_common(repo, interrupted)? != host.git_common(&cwd, interrupted)? {
        return Err("cwd must be this repository or one of its Git worktrees".into());
    }
    if args.text("profile")? != "trusted-local" {
        return Err("agent-supervisor currently accepts trusted-local; use oms runtime backend check for other backends".into());
    }
    let max_log = args.int("max-log-bytes")?;
    if !(1024..=100 * 1024 * 1024).contains(&max_log) {
        return Err("max-log-bytes must be between 1024 and 104857600".into());
    }
    let mut budget = obj([]);
    for (option, key) in [
        ("max-wall-seconds", "max_wall_seconds"),
        ("max-tokens", "max_tokens"),
        ("max-cost-microusd", "max_cost_microusd"),
    ] {
        if args.raw(option).is_some() {
            budget.set(key, V::from(args.int(option)?));
        }
    }
    let provider = args.text("provider")?;
    let provider = if provider.is_empty() {
        "local".to_owned()
    } else {
        provider
    };
    let id = ops::create_attempt(
        repo,
        &ops::StartRequest {
            provider: provider.clone(),
            tool: "agent-supervisor".into(),
            task_id: args.text("task-id")?,
            run_id: args.text("run-id")?,
            budget: budget.clone(),
            refs: obj([("command_sha256", digest.clone().into())]),
            ..Default::default()
        },
        interrupted,
    )?;
    let result = (|| {
        let mut specification = spec(repo, &id)?;
        specification.set("profile", args.text("profile")?);
        specification.set("completion_state", args.text("completion-state")?);
        specification.set("command_sha256", digest.clone());
        ops::storage::write_json_atomic(&ops::attempt_spec_path(repo, &id), &specification, 0o600)?;
        let now = storage::now();
        let mut job = obj([
            ("schema", 1usize.into()),
            ("attempt_id", id.clone().into()),
            ("provider", provider.into()),
            ("profile", args.text("profile")?.into()),
            ("completion_state", args.text("completion-state")?.into()),
            ("task_id", args.text("task-id")?.into()),
            ("argv", argv),
            (
                "cwd",
                cwd.to_str()
                    .ok_or("cwd cannot be encoded as durable UTF-8")?
                    .into(),
            ),
            ("command_sha256", digest.into()),
            ("budget", budget),
            ("runtime_state", "queued".into()),
            (
                "queue_order",
                V::Num(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                        .to_string(),
                ),
            ),
            ("created_at", now.clone().into()),
            ("updated_at", now.into()),
            ("exit_code", V::Null),
            ("max_log_bytes", max_log.into()),
            ("log_bytes", 0usize.into()),
            ("log_truncated", false.into()),
            ("pid", 0usize.into()),
            ("pid_identity", "".into()),
            ("runner_pid", 0usize.into()),
            ("runner_pid_identity", "".into()),
        ]);
        save_new(repo, store, &mut job, interrupted)
    })();
    if let Err(error) = result {
        let _ = life::transition(
            repo,
            &id,
            "blocked",
            "submit_incomplete",
            "submit-incomplete",
            interrupted,
        );
        return Err(error);
    }
    Ok(id)
}
fn public(
    job: &V,
    attempts: &BTreeMap<String, V>,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<V> {
    let projected = attempts.get(text(job, "attempt_id")).unwrap_or(&V::Null);
    let mut row = obj([
        ("schema", 1usize.into()),
        ("attempt_id", job.get("attempt_id").clone()),
        ("state", projected.default("state", "unknown")),
        ("reason_code", projected.default("reason_code", "")),
        (
            "pid_alive",
            process_matches(
                integer(job, "pid"),
                text(job, "pid_identity"),
                host,
                interrupted,
            )?
            .into(),
        ),
        ("resume_mode", "child_attempt".into()),
        ("input_resume_supported", false.into()),
        ("approval_scope", "patch_land_only".into()),
        ("sandbox", false.into()),
        ("supervision_scope", "process_group".into()),
    ]);
    for (key, default) in [
        ("runtime_state", "unknown"),
        ("provider", ""),
        ("profile", ""),
        ("completion_state", "review"),
        ("command_sha256", ""),
        ("created_at", ""),
        ("updated_at", ""),
    ] {
        row.set(key, job.default(key, default));
    }
    row.set("exit_code", job.get("exit_code").clone());
    row.set("max_log_bytes", job.default("max_log_bytes", DEFAULT_LOG));
    row.set("log_bytes", job.default("log_bytes", 0usize));
    row.set("log_truncated", job.get("log_truncated").truth());
    for key in ["usage", "usage_reports", "budget"] {
        row.set(key, projected.default(key, obj([])));
    }
    row.set(
        "parent_attempt_id",
        projected.default("parent_attempt_id", ""),
    );
    Ok(row)
}
fn wait(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
) -> Result<i32> {
    let id = args.text("attempt")?;
    storage::valid_id(&id)?;
    let started = Instant::now();
    let timeout = args.float("timeout")?;
    loop {
        check(interrupted)?;
        let row = public(
            &store.load(&id)?,
            &life::projection(repo)?,
            host,
            interrupted,
        )?;
        let state = text(&row, "state");
        if life::terminal(state) || state == "review" && text(&row, "completion_state") != "done" {
            if args.flag("json") {
                json(out, &row)?;
            } else {
                line(out, format!("{id} {state}"))?;
            }
            return Ok(if matches!(state, "done" | "review") {
                0
            } else {
                1
            });
        }
        if started.elapsed().as_secs_f64() >= timeout {
            return Err(format!("wait timed out while attempt remained {state}"));
        }
        pause(args.float("interval")?, interrupted)?;
        host.drive()?;
    }
}
fn attach(store: &Store, args: &Args, out: &mut dyn Write) -> Result<()> {
    let id = args.text("attempt")?;
    store.load(&id)?;
    let raw = store.read_log(&id)?;
    let text = String::from_utf8_lossy(&raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines: Vec<_> = text.split_terminator('\n').collect();
    if lines.len() > args.int("tail")? as usize {
        lines.drain(..lines.len() - args.int("tail")? as usize);
    }
    for row in lines {
        line(out, row)?;
    }
    Ok(())
}
fn pause(seconds: f64, interrupted: &AtomicBool) -> Result<()> {
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| "invalid wait interval")?;
    let until = Instant::now()
        .checked_add(duration)
        .ok_or("wait interval is too large")?;
    loop {
        check(interrupted)?;
        let now = Instant::now();
        if now >= until {
            return Ok(());
        }
        std::thread::sleep((until - now).min(Duration::from_millis(50)));
    }
}
fn pid(raw: i64) -> Option<rustix::process::Pid> {
    if raw <= 1 || raw > i32::MAX as i64 {
        None
    } else {
        rustix::process::Pid::from_raw(raw as i32)
    }
}
fn process_identity(raw: i64, host: &mut CommandHost, interrupted: &AtomicBool) -> Result<String> {
    if pid(raw).is_none() {
        return Ok(String::new());
    }
    if let Some(identity) = proc_identity(raw as u32) {
        return Ok(identity);
    }
    let raw = host.query(
        vec![
            "ps".into(),
            "-o".into(),
            "lstart=".into(),
            "-p".into(),
            raw.to_string().into(),
        ],
        None,
        interrupted,
    );
    Ok(raw
        .ok()
        .map(|b| storage::digest(b.trim_ascii()))
        .unwrap_or_default())
}
fn process_matches(
    raw: i64,
    expected: &str,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<bool> {
    if expected.is_empty() || pid(raw).is_none() {
        return Ok(false);
    }
    let current = process_identity(raw, host, interrupted)?;
    Ok(!current.is_empty() && current == expected)
}
fn zombie(raw: i64, host: &mut CommandHost, interrupted: &AtomicBool) -> Result<bool> {
    if pid(raw).is_none() {
        return Ok(false);
    }
    if let Some(fields) = ops::lock::proc_stat_fields(raw as u32) {
        return Ok(fields.first().is_some_and(|s| s == "Z" || s == "X"));
    }
    let raw = host.query(
        vec![
            "ps".into(),
            "-o".into(),
            "stat=".into(),
            "-p".into(),
            raw.to_string().into(),
        ],
        None,
        interrupted,
    );
    Ok(raw.ok().is_some_and(|b| b.trim_ascii().starts_with(b"Z")))
}
fn group_alive(raw: i64) -> bool {
    let Some(pid) = pid(raw) else {
        return false;
    };
    if matches!(
        rustix::process::test_kill_process_group(pid),
        Err(rustix::io::Errno::SRCH)
    ) {
        return false;
    }
    crate::process::group_quiescent(pid) != Some(true)
}
fn recorded(
    raw: i64,
    expected: &str,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<bool> {
    let Some(pid) = pid(raw) else {
        return Ok(false);
    };
    Ok(process_matches(raw, expected, host, interrupted)?
        && rustix::process::getpgid(Some(pid)).ok() == Some(pid))
}
fn owner_cancelled(repo: &Path, id: &str, job: &V, interrupted: &AtomicBool) -> Result<bool> {
    let attempts = life::projection(repo)?;
    let attempt = attempts.get(id);
    let settled = attempt.is_some_and(|a| {
        text(a, "state") == "cancelled" && text(a, "reason_code") == "cancelled_by_owner"
    });
    if job.get("finish") == &V::Null {
        return Ok(settled);
    }
    let Ok(facts) = finish_facts(job) else {
        return Ok(false);
    };
    if text(&facts, "state") != "cancelled" || text(&facts, "reason_code") != "cancelled_by_owner" {
        return Ok(false);
    }
    if settled {
        return Ok(true);
    }
    if attempt.is_some_and(|a| life::terminal(text(a, "state"))) {
        return Ok(false);
    }
    life::transition(
        repo,
        id,
        "cancelled",
        "cancelled_by_owner",
        text(&facts, "idempotency_key"),
        interrupted,
    )?;
    Ok(true)
}
fn cancel(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<V> {
    let id = args.text("attempt")?;
    let mut identity_failure = false;
    let (runtime_state, delivery, replayed) = {
        let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
        let mut job = store.load(&id)?;
        let state = text(&job, "runtime_state").to_owned();
        if state == "queued" {
            finish(
                &mut job,
                "cancelled",
                V::Null,
                ("cancelled", "cancelled_by_owner", "owner-cancelled"),
            );
            store.save(&mut job)?;
            life::transition(
                repo,
                &id,
                "cancelled",
                "cancelled_by_owner",
                "owner-cancelled",
                interrupted,
            )?;
            ("cancelled", "confirmed", false)
        } else if ACTIVE.contains(&state.as_str()) {
            let child = integer(&job, "pid");
            if child != 0
                && (ops::lock::pid_alive(child) || group_alive(child))
                && !recorded(child, text(&job, "pid_identity"), host, interrupted)?
            {
                identity_failure = true;
                job.set("runtime_state", "interrupted");
                job.set("exit_code", V::Null);
                job.set("pid", 0usize);
                job.set("pid_identity", "");
                job.set("finished_at", storage::now());
                job.set(
                    "finish",
                    obj([
                        ("schema", 1usize.into()),
                        ("state", "failed".into()),
                        ("reason_code", "process_identity_mismatch".into()),
                        ("idempotency_key", "process-identity-mismatch".into()),
                    ]),
                );
                store.save(&mut job)?;
                ("interrupted", "outcome_unknown", false)
            } else if state == "cancel_requested" {
                ("cancel_requested", "dispatched", true)
            } else {
                job.set("runtime_state", "cancel_requested");
                // The live runner owns the unreaped leader and forwards this
                // monotonic request. An external observer cannot pin a numeric
                // process group across check/kill, so it never signals by number.
                store.save(&mut job)?;
                ("cancel_requested", "dispatched", false)
            }
        } else if state == "cancelled" && owner_cancelled(repo, &id, &job, interrupted)? {
            ("cancelled", "confirmed", true)
        } else if ["finished", "cancelled", "abandoned", "interrupted"].contains(&state.as_str()) {
            return Err(format!("attempt is no longer cancellable: {state}"));
        } else {
            return Err(format!("unknown runtime state: {state}"));
        }
    };
    if identity_failure {
        life::transition(
            repo,
            &id,
            "failed",
            "process_identity_mismatch",
            "process-identity-mismatch",
            interrupted,
        )?;
    }
    Ok(obj([
        ("schema", 1usize.into()),
        ("attempt", id.into()),
        ("runtime_state", runtime_state.into()),
        ("delivery", delivery.into()),
        ("replayed", replayed.into()),
    ]))
}
fn dispatch(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
) -> Result<Vec<String>> {
    if host.runner_argv.first().is_none_or(|s| s.is_empty()) {
        return Err("native supervisor runner program unavailable".into());
    }
    let mut limits = BTreeMap::new();
    for raw in args.values("provider-limit") {
        let raw = raw.to_str().ok_or("provider-limit must be PROVIDER=N")?;
        let (provider, value) = raw
            .split_once('=')
            .ok_or("provider-limit must be PROVIDER=N")?;
        if !ops::projection::id(provider) {
            return Err("provider must match [A-Za-z0-9][A-Za-z0-9._:-]{0,159}".into());
        }
        let limit: i64 = value
            .trim()
            .parse()
            .map_err(|_| "provider-limit must end in an integer")?;
        if limit < 1 {
            return Err("provider-limit must be positive".into());
        }
        limits.insert(provider.to_owned(), limit);
    }
    let mut reserved = Vec::new();
    {
        let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
        let jobs = store.all(true)?;
        let active: Vec<_> = jobs
            .iter()
            .filter(|j| ACTIVE.contains(&text(j, "runtime_state")))
            .collect();
        let mut by_provider: BTreeMap<String, i64> = BTreeMap::new();
        for job in &active {
            *by_provider
                .entry(text(job, "provider").to_owned())
                .or_default() += 1;
        }
        let mut units: i128 = active
            .iter()
            .map(|j| integer(j, "reserved_tokens") as i128)
            .sum();
        let mut money: i128 = active
            .iter()
            .map(|j| integer(j, "reserved_cost_microusd") as i128)
            .sum();
        let mut queued: Vec<_> = jobs
            .iter()
            .filter(|j| text(j, "runtime_state") == "queued")
            .cloned()
            .collect();
        queued.sort_by(|a, b| {
            let order = |v: &V| v.get("queue_order").py().parse::<i128>().unwrap_or(0);
            (order(a), text(a, "created_at"), text(a, "attempt_id")).cmp(&(
                order(b),
                text(b, "created_at"),
                text(b, "attempt_id"),
            ))
        });
        for mut job in queued {
            check(interrupted)?;
            if reserved.len() as i64 >= args.int("max-jobs")?
                || active.len() as i64 + reserved.len() as i64 >= args.int("max-running")?
            {
                break;
            }
            let provider = text(&job, "provider").to_owned();
            if limits
                .get(&provider)
                .is_some_and(|max| by_provider.get(&provider).copied().unwrap_or(0) >= *max)
            {
                continue;
            }
            let budget = job.get("budget");
            let reserve_units = optional_integer(budget, unit_budget_key())?;
            let reserve_money = optional_integer(budget, "max_cost_microusd")?;
            if let Some(max) = optional_arg(args, unit_total_option())?
                && reserve_units.is_none_or(|n| units + n as i128 > max as i128)
            {
                continue;
            }
            if let Some(max) = optional_arg(args, "max-total-cost-microusd")?
                && reserve_money.is_none_or(|n| money + n as i128 > max as i128)
            {
                continue;
            }
            job.set("runtime_state", "launching");
            job.set("launch_started_at", storage::now());
            job.set("launch_nonce", storage::nonce()?);
            job.set("reserved_tokens", reserve_units.unwrap_or(0));
            job.set("reserved_cost_microusd", reserve_money.unwrap_or(0));
            store.save(&mut job)?;
            reserved.push(text(&job, "attempt_id").to_owned());
            *by_provider.entry(provider).or_default() += 1;
            units += reserve_units.unwrap_or(0) as i128;
            money += reserve_money.unwrap_or(0) as i128;
        }
    }
    let mut launched = Vec::new();
    let mut failures = Vec::new();
    for id in reserved {
        {
            let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
            let mut job = store.load(&id)?;
            if text(&job, "runtime_state") == "cancel_requested" {
                finish(
                    &mut job,
                    "cancelled",
                    V::Null,
                    (
                        "cancelled",
                        "cancelled_by_owner",
                        "dispatch-cancelled-before-spawn",
                    ),
                );
                store.save(&mut job)?;
                life::transition(
                    repo,
                    &id,
                    "cancelled",
                    "cancelled_by_owner",
                    "dispatch-cancelled-before-spawn",
                    interrupted,
                )?;
                continue;
            }
        }
        let mut argv = host.runner_argv.clone();
        argv.extend([
            OsString::from("--repo"),
            repo.as_os_str().to_owned(),
            "_run-one".into(),
            "--attempt".into(),
            id.clone().into(),
        ]);
        let mut request = Request::new(argv);
        request.scope = Scope::Leader;
        request.capture.max_bytes = 0;
        request.environment.push((
            "XDG_STATE_HOME".into(),
            store.state_home.as_os_str().to_owned(),
        ));
        let handle = match host.reactor.start_detached(request) {
            Ok(handle) => handle,
            Err(_) => {
                let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
                let mut job = store.load(&id)?;
                finish(
                    &mut job,
                    "interrupted",
                    V::Null,
                    (
                        "blocked",
                        "runner_launch_failed",
                        "dispatch-runner-launch-failed",
                    ),
                );
                store.save(&mut job)?;
                life::transition(
                    repo,
                    &id,
                    "blocked",
                    "runner_launch_failed",
                    "dispatch-runner-launch-failed",
                    interrupted,
                )?;
                failures.push(id);
                continue;
            }
        };
        host.retained.push(handle);
        let child = host.reactor.snapshot(handle)?.pid;
        {
            let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
            let mut job = store.load(&id)?;
            if text(&job, "runtime_state") == "launching" {
                job.set("runner_pid", child as i64);
                job.set(
                    "runner_pid_identity",
                    process_identity(child as i64, host, interrupted)?,
                );
                store.save(&mut job)?;
            }
        }
        host.detached.push(handle);
        launched.push(id.clone());
        line(out, id)?;
        out.flush().map_err(|e| e.to_string())?;
    }
    if failures.is_empty() {
        Ok(launched)
    } else {
        Err(format!(
            "could not launch runner for: {}",
            failures.join(", ")
        ))
    }
}
fn optional_integer(row: &V, key: &str) -> Result<Option<i64>> {
    if row.get(key) == &V::Null {
        Ok(None)
    } else {
        row.get(key)
            .py()
            .parse()
            .map(Some)
            .map_err(|_| format!("invalid supervisor integer: {key}"))
    }
}
fn optional_arg(args: &Args, key: &str) -> Result<Option<i64>> {
    if args.raw(key).is_none() {
        Ok(None)
    } else {
        args.int(key).map(Some)
    }
}
fn resume(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<String> {
    let parent_id = args.text("attempt")?;
    let parent_job = store.load(&parent_id)?;
    let attempts = life::projection(repo)?;
    let parent = attempts
        .get(&parent_id)
        .ok_or("attempt is still live; attach or wait instead")?;
    if !life::terminal(text(parent, "state")) && text(parent, "state") != "blocked" {
        return Err("attempt is still live; attach or wait instead".into());
    }
    let prior = spec(repo, &parent_id)?;
    if !text(&prior, "base_sha").is_empty() {
        let head = host
            .query(
                vec![
                    "git".into(),
                    "-C".into(),
                    repo.as_os_str().into(),
                    "rev-parse".into(),
                    "HEAD".into(),
                ],
                None,
                interrupted,
            )
            .unwrap_or_default();
        if trim_line(&head) != text(&prior, "base_sha").as_bytes() {
            return Err("repository HEAD changed; the prior job cannot be resumed".into());
        }
    }
    let id = ops::create_attempt(
        repo,
        &ops::StartRequest {
            provider: parent_job.default("provider", "local").py(),
            tool: "agent-supervisor".into(),
            task_id: text(&parent_job, "task_id").into(),
            run_id: text(&prior, "run_id").into(),
            parent_attempt_id: parent_id.clone(),
            budget: parent_job.default("budget", obj([])),
            refs: obj([("command_sha256", parent_job.get("command_sha256").clone())]),
            ..Default::default()
        },
        interrupted,
    )?;
    let mut new_spec = spec(repo, &id)?;
    for key in ["profile", "completion_state", "command_sha256"] {
        new_spec.set(key, parent_job.get(key).clone());
    }
    ops::storage::write_json_atomic(&ops::attempt_spec_path(repo, &id), &new_spec, 0o600)?;
    let mut job = parent_job;
    job.set("attempt_id", id.clone());
    job.set("runtime_state", "queued");
    job.set(
        "queue_order",
        V::Num(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_string(),
        ),
    );
    job.set("created_at", storage::now());
    job.set("updated_at", storage::now());
    job.set("exit_code", V::Null);
    for key in ["pid", "runner_pid"] {
        job.set(key, 0usize);
    }
    for key in ["pid_identity", "runner_pid_identity"] {
        job.set(key, "");
    }
    job.set("parent_attempt_id", parent_id);
    if let V::Obj(ref mut m) = job {
        for key in [
            "finished_at",
            "finish",
            "reserved_tokens",
            "reserved_cost_microusd",
            "launch_started_at",
            "launch_nonce",
        ] {
            m.remove(key.as_bytes());
        }
    }
    save_new(repo, store, &mut job, interrupted)?;
    Ok(id)
}
fn age(stamp: &str) -> Option<i64> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(seconds - ops::parse_ts(stamp).ok()?.timestamp())
}
fn stop_orphan_group(
    raw: i64,
    expected: &str,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
) -> Result<bool> {
    let Some(pid) = pid(raw) else {
        return Ok(true);
    };
    for (signal, grace) in [
        (rustix::process::Signal::TERM, Duration::from_secs(2)),
        (rustix::process::Signal::KILL, Duration::from_secs(3)),
    ] {
        if !group_alive(raw) {
            break;
        }
        check(interrupted)?;
        // Match the canonical reconcile policy: validate start identity and
        // group leadership immediately before each bounded signal attempt.
        if !recorded(raw, expected, host, interrupted)? {
            return Ok(false);
        }
        match rustix::process::kill_process_group(pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) => {}
            Err(error) => return Err(format!("cannot signal orphaned supervised group: {error}")),
        }
        let until = Instant::now() + grace;
        while group_alive(raw) && Instant::now() < until {
            pause(0.05, interrupted)?;
        }
    }
    if group_alive(raw) {
        return Ok(false);
    }
    // A separate CLI cannot reap another parent's child. If this library host
    // did adopt the recorded leader, consume only its exact wait status, after
    // all group signaling decisions are finished.
    if process_matches(raw, expected, host, interrupted)? {
        use rustix::process::{WaitId, WaitIdOptions};
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        ) {
            Ok(_) | Err(rustix::io::Errno::CHILD | rustix::io::Errno::SRCH) => {}
            Err(error) => return Err(format!("orphaned leader reap failed: {error}")),
        }
    }
    Ok(true)
}
fn reconcile(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
) -> Result<()> {
    let mut selected = Vec::new();
    let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
    let jobs = store.all(true)?;
    let ids: BTreeSet<_> = jobs
        .iter()
        .map(|j| text(j, "attempt_id").to_owned())
        .collect();
    let mut attempts = life::projection(repo)?;
    for (id, attempt) in &attempts {
        if text(attempt, "state") == "queued"
            && text(attempt, "tool") == "agent-supervisor"
            && !ids.contains(id)
            && age(text(attempt, "created_at")).is_some_and(|s| s >= 10)
            && !store.exists(id)?
        {
            selected.push(V::from(id.clone()));
            if args.flag("apply") {
                match life::transition(
                    repo,
                    id,
                    "blocked",
                    "runtime_job_missing",
                    "reconcile-runtime-job-missing",
                    interrupted,
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        let current = life::projection(repo)?;
                        if current
                            .get(id)
                            .is_some_and(|a| text(a, "state") == "queued")
                        {
                            return Err(e);
                        }
                    }
                }
            }
        }
    }
    for mut job in jobs {
        check(interrupted)?;
        let state = text(&job, "runtime_state").to_owned();
        if ["finished", "cancelled", "interrupted", "abandoned"].contains(&state.as_str()) {
            let id = text(&job, "attempt_id");
            let attempt = attempts.get(id).unwrap_or(&V::Null);
            if settlement_pending(&job, attempt)?
                && !process_matches(
                    integer(&job, "runner_pid"),
                    text(&job, "runner_pid_identity"),
                    host,
                    interrupted,
                )?
                && !process_matches(
                    integer(&job, "pid"),
                    text(&job, "pid_identity"),
                    host,
                    interrupted,
                )?
                && !group_alive(integer(&job, "pid"))
            {
                selected.push(id.into());
                if args.flag("apply") {
                    settle_finish(repo, &job, interrupted)?;
                }
            }
            continue;
        }
        if !ACTIVE.contains(&state.as_str()) {
            continue;
        }
        let runner = integer(&job, "runner_pid");
        if process_matches(runner, text(&job, "runner_pid_identity"), host, interrupted)?
            && !zombie(runner, host, interrupted)?
        {
            continue;
        }
        if state == "launching"
            && runner == 0
            && age(text(&job, "launch_started_at")).unwrap_or(999) < 10
        {
            continue;
        }
        let child = integer(&job, "pid");
        let alive = group_alive(child);
        let matches = recorded(child, text(&job, "pid_identity"), host, interrupted)?;
        let mismatch = child != 0 && (ops::lock::pid_alive(child) || alive) && !matches;
        let id = text(&job, "attempt_id").to_owned();
        selected.push(id.clone().into());
        if !args.flag("apply") {
            continue;
        }
        if mismatch && !alive {
            finish(
                &mut job,
                "interrupted",
                V::Null,
                (
                    "failed",
                    "process_identity_mismatch",
                    "process-identity-mismatch",
                ),
            );
            store.save(&mut job)?;
            attempts = life::projection(repo)?;
            if attempts
                .get(&id)
                .is_some_and(|a| life::nonterminal(text(a, "state")))
            {
                life::transition(
                    repo,
                    &id,
                    "failed",
                    "process_identity_mismatch",
                    "process-identity-mismatch",
                    interrupted,
                )?;
            }
            continue;
        }
        if alive {
            job.set("runtime_state", "cleanup_failed");
            job.set("runner_pid", 0usize);
            job.set("runner_pid_identity", "");
            store.save(&mut job)?;
            let stopped = if matches {
                stop_orphan_group(child, text(&job, "pid_identity"), host, interrupted)
            } else {
                Ok(false)
            };
            if !matches!(stopped, Ok(true)) {
                if attempts.get(&id).is_some_and(|a| {
                    life::nonterminal(text(a, "state")) && text(a, "state") != "blocked"
                }) {
                    life::transition(
                        repo,
                        &id,
                        "blocked",
                        "orphan_cleanup_pending",
                        "reconcile-cleanup-pending",
                        &AtomicBool::new(false),
                    )?;
                }
                stopped?;
                continue;
            }
        }
        let cancelled = state == "cancel_requested";
        finish(
            &mut job,
            if cancelled {
                "cancelled"
            } else {
                "interrupted"
            },
            V::Null,
            if cancelled {
                (
                    "cancelled",
                    "cancelled_by_owner",
                    "reconcile-runner-cancelled",
                )
            } else {
                ("blocked", "runner_lost", "reconcile-runner-lost")
            },
        );
        store.save(&mut job)?;
        attempts = life::projection(repo)?;
        if attempts
            .get(&id)
            .is_some_and(|a| life::nonterminal(text(a, "state")) && text(a, "state") != "blocked")
        {
            life::transition(
                repo,
                &id,
                if cancelled { "cancelled" } else { "blocked" },
                if cancelled {
                    "cancelled_by_owner"
                } else {
                    "runner_lost"
                },
                if cancelled {
                    "reconcile-runner-cancelled"
                } else {
                    "reconcile-runner-lost"
                },
                interrupted,
            )?;
        }
    }
    if args.flag("json") {
        json(out, &V::Arr(selected))?;
    } else if selected.is_empty() {
        line(out, "no interrupted attempts")?;
    } else {
        for id in selected {
            line(out, id.py())?;
        }
    }
    Ok(())
}
fn gc(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
) -> Result<()> {
    let mut selected = Vec::new();
    let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
    let attempts = life::projection(repo)?;
    for job in store.all(false)? {
        let id = text(&job, "attempt_id");
        let state = attempts
            .get(id)
            .map(|a| text(a, "state"))
            .unwrap_or("unknown");
        let runtime = text(&job, "runtime_state");
        if !life::terminal(state) || !["finished", "cancelled", "abandoned"].contains(&runtime) {
            continue;
        }
        if process_matches(
            integer(&job, "pid"),
            text(&job, "pid_identity"),
            host,
            interrupted,
        )? || process_matches(
            integer(&job, "runner_pid"),
            text(&job, "runner_pid_identity"),
            host,
            interrupted,
        )? {
            continue;
        }
        let stamp = ["finished_at", "updated_at", "created_at"]
            .iter()
            .map(|k| text(&job, k))
            .find(|s| !s.is_empty())
            .unwrap_or("");
        if age(stamp).is_none_or(|s| s < args.int("older-than-days").unwrap_or(30) * 86400) {
            continue;
        }
        selected.push(obj([
            ("attempt_id", id.into()),
            ("terminal", true.into()),
            ("lifecycle_state", state.into()),
            ("runtime_state", runtime.into()),
            ("updated_at", stamp.into()),
            ("applied", args.flag("apply").into()),
        ]));
        if args.flag("apply") {
            store.remove(id)?;
        }
    }
    selected.sort_by(|a, b| text(a, "attempt_id").cmp(text(b, "attempt_id")));
    if args.flag("json") {
        json(out, &V::Arr(selected))?;
    } else if selected.is_empty() {
        line(out, "no expired terminal runtime records")?;
    } else {
        for row in selected {
            line(
                out,
                format!(
                    "{}: {}",
                    if args.flag("apply") {
                        "deleted"
                    } else {
                        "would delete"
                    },
                    text(&row, "attempt_id")
                ),
            )?;
        }
    }
    Ok(())
}
fn serve(
    repo: &Path,
    store: &Store,
    args: &Args,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    out: &mut dyn Write,
) -> Result<()> {
    let started = Instant::now();
    let mut launched = 0usize;
    while launched < args.int("max-jobs")? as usize
        && started.elapsed().as_secs() < args.int("wall-seconds")? as u64
    {
        let remaining = args.int("max-jobs")? as usize - launched;
        let mut dispatch_args = vec![
            OsString::from("dispatch"),
            "--max-running".into(),
            args.text("max-running")?.into(),
            "--max-jobs".into(),
            remaining
                .min(args.int("max-running")? as usize)
                .to_string()
                .into(),
        ];
        for key in [
            "provider-limit",
            "max-total-tokens",
            "max-total-cost-microusd",
        ] {
            for value in args.values(key) {
                dispatch_args.extend([format!("--{key}").into(), value.clone()]);
            }
        }
        let Parsed::Args(dispatch_args) = cli::parse(&dispatch_args)? else {
            unreachable!()
        };
        launched += dispatch(repo, store, &dispatch_args, host, interrupted, out)?.len();
        let jobs = store.all(true)?;
        let queued = jobs.iter().any(|j| text(j, "runtime_state") == "queued");
        let active = jobs
            .iter()
            .any(|j| ACTIVE.contains(&text(j, "runtime_state")));
        if !queued && (!active || !args.flag("until-idle")) {
            break;
        }
        host.drive()?;
        pause(args.float("interval")?, interrupted)?;
    }
    Ok(())
}
fn runner(
    repo: &Path,
    store: &Store,
    id: &str,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    err: &mut dyn Write,
) -> Result<i32> {
    match rustix::process::setsid() {
        Ok(_) => {}
        Err(rustix::io::Errno::PERM)
            if rustix::process::getpgid(None).ok() == Some(rustix::process::getpid()) => {}
        Err(error) => {
            return Err(format!(
                "native runner could not establish its session: {error}"
            ));
        }
    }
    storage::valid_id(id)?;
    let cleanup = AtomicBool::new(false);
    let mut job;
    {
        let _queue = FileLock::acquire(&store.queue_path(), interrupted)?;
        job = store.load(id)?;
        if text(&job, "runtime_state") == "cancel_requested" {
            finish(
                &mut job,
                "cancelled",
                V::Null,
                (
                    "cancelled",
                    "cancelled_by_owner",
                    "runner-cancelled-before-launch",
                ),
            );
            store.save(&mut job)?;
            life::transition(
                repo,
                id,
                "cancelled",
                "cancelled_by_owner",
                "runner-cancelled-before-launch",
                &cleanup,
            )?;
            return Ok(130);
        }
        if !["queued", "launching"].contains(&text(&job, "runtime_state")) {
            return Ok(0);
        }
        if hard_budget(&job) {
            finish(
                &mut job,
                "finished",
                78i64.into(),
                (
                    "failed",
                    "hard_budget_unsupported",
                    "runner-hard-budget-unsupported",
                ),
            );
            store.save(&mut job)?;
            life::transition(
                repo,
                id,
                "failed",
                "hard_budget_unsupported",
                "runner-hard-budget-unsupported",
                &cleanup,
            )?;
            return Ok(78);
        }
        job.set("runtime_state", "starting");
        job.set("runner_pid", std::process::id() as i64);
        job.set(
            "runner_pid_identity",
            process_identity(std::process::id() as i64, host, &cleanup)?,
        );
        store.save(&mut job)?;
    }
    let result = runner_start(repo, store, id, job, host, interrupted, err);
    if let Err(error) = result {
        // runner_start retains every spawned process before returning an error.
        // A pre-spawn failure has no child, and is safe to mark interrupted.
        let _queue = FileLock::acquire(&store.queue_path(), &cleanup)?;
        let mut current = store.load(id)?;
        if current.get("finish") != &V::Null {
            // A completed runner may fail while publishing lifecycle events.
            // Its durable outcome remains the reconciliation authority.
            settle_finish(repo, &current, &cleanup)?;
            return Err(error);
        }
        if text(&current, "runtime_state") != "cleanup_failed" {
            finish(
                &mut current,
                "interrupted",
                V::Null,
                ("blocked", "supervisor_error", "runner-interrupted"),
            );
            store.save(&mut current)?;
        }
        if life::projection(repo)?
            .get(id)
            .is_some_and(|a| life::nonterminal(text(a, "state")))
        {
            life::transition(
                repo,
                id,
                "blocked",
                "supervisor_error",
                "runner-interrupted",
                &cleanup,
            )?;
        }
        return Err(error);
    }
    result
}
fn hard_budget(job: &V) -> bool {
    ["max_tokens", "max_cost_microusd"]
        .iter()
        .any(|key| job.get("budget").has(key))
}
fn runner_start(
    repo: &Path,
    store: &Store,
    id: &str,
    job: V,
    host: &mut CommandHost,
    interrupted: &AtomicBool,
    err: &mut dyn Write,
) -> Result<i32> {
    let cleanup = AtomicBool::new(false);
    life::transition(repo, id, "starting", "", "runner-starting", interrupted)?;
    let specification = spec(repo, id)?;
    let values = job
        .get("argv")
        .array()
        .iter()
        .map(|s| {
            s.text()
                .map(OsString::from)
                .ok_or("invalid durable command argument")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let (values, _, digest) = command(&values)?;
    if digest != text(&job, "command_sha256")
        || digest != text(&specification, "command_sha256")
        || specification.get("refs").get("command_sha256").text() != Some(&digest)
    {
        return Err("submitted command digest does not match immutable lifecycle intent".into());
    }
    if text(&job, "profile") != "trusted-local"
        || text(&specification, "profile") != "trusted-local"
        || job.get("completion_state") != specification.get("completion_state")
    {
        return Err("submitted execution profile or completion contract changed".into());
    }
    let cwd = PathBuf::from(text(&job, "cwd"));
    let cwd = std::fs::canonicalize(cwd).map_err(|e| e.to_string())?;
    if !cwd.is_dir() || host.git_common(repo, interrupted)? != host.git_common(&cwd, interrupted)? {
        return Err("cwd must be this repository or one of its Git worktrees".into());
    }
    let max_log = optional_integer(&job, "max_log_bytes")?.unwrap_or(DEFAULT_LOG as i64);
    if !(1024..=100 * 1024 * 1024).contains(&max_log) {
        return Err("max-log-bytes must be between 1024 and 104857600".into());
    }
    let mut log = store.log(id, max_log as usize)?;
    let mut request = Request::new(values);
    request.cwd = Some(cwd);
    request.capture.max_bytes = max_log as usize;
    if let Some(wall) = optional_integer(job.get("budget"), "max_wall_seconds")? {
        if wall < 0 {
            return Err("invalid negative wall budget".into());
        }
        request.deadline = Some(
            Instant::now()
                .checked_add(Duration::from_secs(wall as u64))
                .ok_or("wall budget is too large")?,
        );
    }
    request.environment.extend([
        ("OMS_ATTEMPT_ID".into(), id.into()),
        ("OMS_ATTEMPT_SUPERVISED".into(), "1".into()),
        ("OMS_EXECUTION_PROFILE".into(), "trusted-local".into()),
    ]);
    #[cfg(target_os = "linux")]
    let mut reactor = Reactor::dedicated()?;
    #[cfg(target_os = "linux")]
    {
        request.scope = Scope::WholeTree;
    }
    #[cfg(not(target_os = "linux"))]
    let mut reactor = Reactor::new();
    #[cfg(not(target_os = "linux"))]
    {
        request.scope = Scope::ProcessGroup;
    }
    let handle = match reactor.start(request) {
        Ok(handle) => handle,
        Err(crate::process::StartError::DeadlineExpired) => {
            store.finish_log(&mut log, b"")?;
            let _queue = FileLock::acquire(&store.queue_path(), &cleanup)?;
            let mut job = store.load(id)?;
            finish(
                &mut job,
                "finished",
                124i64.into(),
                ("timed_out", "wall_budget_exceeded", "runner-timed-out"),
            );
            store.save(&mut job)?;
            life::transition(
                repo,
                id,
                "timed_out",
                "wall_budget_exceeded",
                "runner-timed-out",
                &cleanup,
            )?;
            return Ok(124);
        }
        Err(message) => {
            let raw = match &message {
                crate::process::StartError::Launch(_) => format!("{message}\n"),
                _ => format!("launch failed: {message}\n"),
            }
            .into_bytes();
            let bytes = &raw[..raw.len().min(max_log as usize)];
            store.finish_log(&mut log, bytes)?;
            let _queue = FileLock::acquire(&store.queue_path(), &cleanup)?;
            let mut job = store.load(id)?;
            finish(
                &mut job,
                "finished",
                127i64.into(),
                ("failed", "launch_failed", "runner-launch-failed"),
            );
            job.set("log_bytes", bytes.len());
            job.set("log_truncated", raw.len() > max_log as usize);
            store.save(&mut job)?;
            life::transition(
                repo,
                id,
                "failed",
                "launch_failed",
                "runner-launch-failed",
                &cleanup,
            )?;
            return Ok(127);
        }
    };
    let mut active = Some(handle);
    let mut observed = None;
    let mut cancelled = false;
    let mut identity_failed = false;
    let mut stop_started = None;
    let mut child_identity = String::new();
    let result: Result<i32> = (|| {
        let child = reactor.snapshot(handle)?.pid;
        child_identity = process_identity(child as i64, host, &cleanup)?;
        if child_identity.is_empty() {
            return Err("could not establish supervised process identity".into());
        }
        {
            let _queue = FileLock::acquire(&store.queue_path(), &cleanup)?;
            let mut current = store.load(id)?;
            cancelled = text(&current, "runtime_state") == "cancel_requested";
            if !cancelled && text(&current, "runtime_state") != "starting" {
                return Err(format!(
                    "attempt changed state during launch: {}",
                    text(&current, "runtime_state")
                ));
            }
            current.set("pid", child as i64);
            current.set("pid_identity", child_identity.clone());
            if !cancelled {
                current.set("runtime_state", "running");
            }
            store.save(&mut current)?;
        }
        if cancelled {
            reactor.stop(handle, StopReason::Cancelled)?;
            stop_started = Some(Instant::now());
        } else {
            life::transition(repo, id, "working", "", "runner-working", &cleanup)?;
        }
        let mut heartbeat = Instant::now() + Duration::from_secs(10);
        loop {
            let current = store.load(id)?;
            cancelled |= text(&current, "runtime_state") == "cancel_requested";
            identity_failed |= text(&current, "runtime_state") == "interrupted";
            if cancelled || identity_failed || interrupted.load(Ordering::Relaxed) {
                reactor.stop(
                    handle,
                    if cancelled {
                        StopReason::Cancelled
                    } else {
                        StopReason::SupervisorError
                    },
                )?;
                stop_started.get_or_insert_with(Instant::now);
            }
            if let Some(signal) = host.received_signal.and_then(|get| get()) {
                reactor.stop(handle, StopReason::Signal(signal))?;
                stop_started.get_or_insert_with(Instant::now);
            }
            if Instant::now() >= heartbeat {
                let epoch = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    / 10;
                life::report(
                    repo,
                    id,
                    "attempt.heartbeat",
                    None,
                    &format!("heartbeat-{epoch}"),
                    &cleanup,
                )?;
                heartbeat = Instant::now() + Duration::from_secs(10);
            }
            match reactor.poll(handle, Duration::from_millis(50))? {
                Progress::Pending(snapshot) => {
                    store.append_log(&mut log, reactor.stdout_prefix(handle)?)?;
                    if snapshot.stop.is_some() || snapshot.exit.is_some() {
                        stop_started.get_or_insert_with(Instant::now);
                    }
                    if stop_started.is_some_and(|t| t.elapsed() >= Duration::from_secs(8)) {
                        return Err(
                            "supervised cleanup remains pending; leader ownership retained".into(),
                        );
                    }
                }
                Progress::Complete(result) => {
                    active = None;
                    observed = Some(result);
                    break;
                }
            }
        }
        let execution = observed
            .as_ref()
            .ok_or("owned execution result unavailable")?;
        store.finish_log(&mut log, &execution.stdout.bytes)?;
        if !execution.cleanup_complete() {
            return Err(execution
                .supervision_error
                .clone()
                .unwrap_or_else(|| "supervised cleanup incomplete".into()));
        }
        if execution.stop.is_none() && !execution.output_complete() {
            return Err("supervised output did not reach EOF".into());
        }
        let duration = execution.duration.as_millis().min(i64::MAX as u128) as i64;
        life::report(
            repo,
            id,
            "attempt.usage",
            Some(obj([("duration_ms", duration.into())])),
            "runner-duration",
            &cleanup,
        )?;
        let exit = execution
            .exit
            .ok_or("supervised leader outcome unavailable")?;
        let code = exit.code.unwrap_or_else(|| -exit.signal.unwrap_or(0));
        let mut final_job;
        {
            let _queue = FileLock::acquire(&store.queue_path(), &cleanup)?;
            final_job = store.load(id)?;
            cancelled |= text(&final_job, "runtime_state") == "cancel_requested";
            identity_failed |= text(&final_job, "runtime_state") == "interrupted";
            let settlement = if execution.stop == Some(StopReason::Deadline) {
                ("timed_out", "wall_budget_exceeded", "runner-timed-out")
            } else if cancelled {
                ("cancelled", "cancelled_by_owner", "runner-cancelled")
            } else if identity_failed {
                (
                    "failed",
                    "process_identity_mismatch",
                    "process-identity-mismatch",
                )
            } else if execution.stop.is_some() {
                ("blocked", "supervisor_error", "runner-interrupted")
            } else if code != 0 {
                ("failed", "command_failed", "runner-command-failed")
            } else if life::projection(repo)?.get(id).is_some_and(hard_budget) {
                (
                    "failed",
                    "hard_budget_unsupported",
                    "runner-hard-budget-unsupported",
                )
            } else if text(&final_job, "completion_state") == "done" {
                ("done", "", "runner-done")
            } else {
                ("review", "", "runner-review")
            };
            finish(
                &mut final_job,
                if identity_failed {
                    "interrupted"
                } else if cancelled {
                    "cancelled"
                } else {
                    "finished"
                },
                (code as i64).into(),
                settlement,
            );
            final_job.set("log_bytes", execution.stdout.bytes.len());
            final_job.set(
                "log_truncated",
                execution.stdout.limited || !execution.stdout.eof,
            );
            store.save(&mut final_job)?;
        }
        settle_finish(repo, &final_job, &cleanup)?;
        if execution.stop == Some(StopReason::Deadline) {
            return Ok(124);
        }
        if cancelled {
            return Ok(130);
        }
        if identity_failed {
            return Ok(78);
        }
        if execution.stop.is_some() {
            return Err("supervised command interrupted before completion".into());
        }
        if execution.stdout.limited {
            let warning = format!(
                "attempt {id}: log hit the {max_log}-byte cap ({} bytes produced); head and tail are kept, the middle was dropped — the in-log marker carries the dropped byte count",
                execution.stdout.observed_bytes
            );
            if let Err(error) = writeln!(err, "{warning}")
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(error.to_string());
            }
        }
        if code != 0 {
            return Ok(code);
        }
        if text(final_job.get("finish"), "reason_code") == "hard_budget_unsupported" {
            return Ok(78);
        }
        Ok(0)
    })();
    if result.is_err() {
        if let Some(handle) = active {
            let _ = reactor.stop(handle, StopReason::SupervisorError);
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                match reactor.poll(handle, Duration::from_millis(50)) {
                    Ok(Progress::Complete(result)) => {
                        observed = Some(result);
                        active = None;
                        break;
                    }
                    Ok(Progress::Pending(_)) => {
                        if let Ok(prefix) = reactor.stdout_prefix(handle) {
                            let _ = store.append_log(&mut log, prefix);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        if let Some(execution) = &observed {
            let _ = store.finish_log(&mut log, &execution.stdout.bytes);
        }
        let snapshot = active.and_then(|h| reactor.snapshot(h).ok());
        let saved = (|| {
            let _queue = FileLock::acquire(&store.queue_path(), &cleanup)?;
            let mut current = store.load(id)?;
            let done =
                active.is_none() && observed.as_ref().is_some_and(Completion::cleanup_complete);
            if done && current.get("finish") != &V::Null {
                return Ok(());
            }
            if done {
                finish(
                    &mut current,
                    "interrupted",
                    observed
                        .as_ref()
                        .and_then(|e| e.exit)
                        .map(|e| V::from(e.code.unwrap_or_else(|| -e.signal.unwrap_or(0)) as i64))
                        .unwrap_or(V::Null),
                    ("blocked", "supervisor_error", "runner-interrupted"),
                );
            } else {
                current.set("runtime_state", "cleanup_failed");
                if let V::Obj(ref mut fields) = current {
                    fields.remove(b"finish".as_slice());
                }
                if let Some(snapshot) = &snapshot {
                    current.set("pid", snapshot.pid as i64);
                    current.set("pid_identity", child_identity.clone());
                }
                // Keep identity even after an uncertain observation. Reconcile
                // may discard mismatches, but cannot turn uncertainty to success.
                current.set("runner_pid", 0usize);
                current.set("runner_pid_identity", "");
                current.set("finished_at", storage::now());
            }
            store.save(&mut current)
        })();
        if let Some(handle) = active {
            host.foreground_cleanup |= saved.is_err();
            host.trees.push((reactor, handle));
        }
        saved?;
    }
    result
}

fn unit_budget_key() -> &'static str {
    "max_tokens"
}
fn unit_total_option() -> &'static str {
    "max-total-tokens"
}

fn proc_identity(raw: u32) -> Option<String> {
    let fields = ops::lock::proc_stat_fields(raw)?;
    let ticks = fields.get(19)?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    if ticks.is_empty() || !ticks.bytes().all(|b| b.is_ascii_digit()) || boot.trim().is_empty() {
        return None;
    }
    Some(storage::digest(
        format!("{}:{ticks}", boot.trim()).as_bytes(),
    ))
}
