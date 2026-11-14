use crate::{Context, Error, ExecutionRecord, Observation, Result, RunLink, Service, Services};
use crate::{metrics, process, store, value::*};
use chrono::{DateTime, Utc};
use oms_context::Json;
use std::collections::{BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const HELP: &str = include_str!("ledger-help.txt");
pub fn root(ctx: &Context<'_>, cwd: &Path) -> Result<PathBuf> {
    let argv = vec![
        OsString::from("git"),
        OsString::from("-C"),
        cwd.as_os_str().to_owned(),
        OsString::from("rev-parse"),
        OsString::from("--show-toplevel"),
    ];
    let got = process::query(ctx, &argv, 65536)?;
    if got.code == 0 {
        let mut bytes = got.bytes;
        while bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        use std::os::unix::ffi::OsStringExt;
        return PathBuf::from(OsString::from_vec(bytes))
            .canonicalize()
            .map_err(|_| Error::io("resolving repository root"));
    }
    if got.code == 124 || got.code >= 129 {
        return Err(Error {
            code: got.code,
            message: "repository query interrupted or timed out".into(),
        });
    }
    if cwd
        .ancestors()
        .any(|p| p.join(".git").symlink_metadata().is_ok())
    {
        return Err(Error::usage(
            "cannot resolve existing Git repository; state is unknown",
        ));
    }
    cwd.canonicalize()
        .map_err(|_| Error::io("resolving working directory"))
}
fn contract_root(ctx: &Context<'_>, ledger: &Path) -> Result<PathBuf> {
    let mut ancestor = ledger.parent().unwrap_or(&ctx.cwd);
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::usage("ledger has no existing ancestor"))?;
    }
    root(ctx, ancestor)
}
pub fn journal_enabled(ctx: &Context<'_>) -> bool {
    !["0", "false", "FALSE", "no", "NO", "off", "OFF"]
        .contains(&ctx.setting("OMS_WORK_JOURNAL").as_str())
        && ctx.setting("OMS_WORK_JOURNAL_ACTIVE") != "1"
        && ctx.setting("OMS_WORK_JOURNAL_SUPPRESS") != "1"
}
pub fn require(services: &dyn Services, kind: Service, name: &str) -> Result<()> {
    if services.available(kind) {
        Ok(())
    } else {
        Err(Error::usage(format!(
            "native {name} service is required; no legacy fallback"
        )))
    }
}
pub fn observe(
    ctx: &Context<'_>,
    services: &mut dyn Services,
    repo: &Path,
    path: &Path,
    row: &Json,
    source: &str,
    err: &mut dyn Write,
) -> Result<()> {
    if journal_enabled(ctx) {
        let event = ctx.setting("OMS_WORK_JOURNAL_EVENT_TYPE");
        if services
            .observe(Observation {
                repo,
                source_type: source,
                record_path: path,
                row,
                event_type: if source == "run-ledger" && !event.is_empty() {
                    Some(&event)
                } else {
                    None
                },
            })
            .is_err()
        {
            writeln!(
                err,
                "warning: Work Journal observer degraded; primary lifecycle result is unchanged"
            )?;
        }
    }
    Ok(())
}
fn sensitive(ctx: &Context<'_>, bytes: &[u8]) -> Result<bool> {
    let mut env = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    env.extend(ctx.env.clone());
    let pairs = env
        .iter()
        .map(|(k, v)| (k.as_bytes(), v.as_bytes()))
        .collect::<Vec<_>>();
    oms_scrub::scan(bytes, &pairs)
        .map(|hits| !hits.is_empty())
        .map_err(|_| Error::usage("sensitive-content scan failed"))
}
fn natural(raw: &OsStr, zero: bool) -> Result<usize> {
    let text = raw.to_str().unwrap_or("");
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::usage("N must be a positive integer"));
    }
    let n = text
        .parse::<usize>()
        .map_err(|_| Error::usage("N must be a positive integer"))?;
    if n == 0 && !zero {
        return Err(Error::usage("N must be a positive integer"));
    }
    Ok(n)
}
fn arg(args: &[OsString], i: &mut usize, name: &str) -> Result<OsString> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Error::usage(format!("{name} requires a value")))
}
fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 60
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn now() -> String {
    DateTime::<Utc>::from(SystemTime::now())
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}
fn effective_run(ctx: &Context<'_>, repo: &Path) -> Result<String> {
    let explicit = ctx.setting("OMS_RUN_ID");
    if !explicit.is_empty() {
        return Ok(explicit.replace('\r', ""));
    }
    let bytes = store::safe_read_in(&repo.join(".oms/runs/CURRENT"), 65536, true, repo)?;
    let line = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let words = line.split_whitespace().collect::<Vec<_>>();
    if words.len() < 2 {
        return Ok(String::new());
    }
    let Ok(stamp) = words[1].parse::<u64>() else {
        return Ok(String::new());
    };
    let ttl = ctx
        .setting("OMS_RUN_CURRENT_TTL")
        .parse::<u64>()
        .unwrap_or(86400);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if t.saturating_sub(stamp) <= ttl {
        Ok(words[0].replace('\r', ""))
    } else {
        Ok(String::new())
    }
}
struct Options {
    file: Option<PathBuf>,
    note: OsString,
    metrics: Option<PathBuf>,
    reason: OsString,
    seed: String,
    group: String,
    gate: bool,
    command: Vec<OsString>,
}
fn options(args: &[OsString], ctx: &Context<'_>) -> Result<Options> {
    let mut o = Options {
        file: None,
        note: OsString::new(),
        metrics: None,
        reason: ctx
            .variable("OMS_RUN_LEDGER_GATE_REASON")
            .unwrap_or_default(),
        seed: String::new(),
        group: String::new(),
        gate: ctx
            .variable("OMS_RUN_LEDGER_GATE")
            .filter(|s| !s.is_empty())
            .is_none_or(|s| s == "1"),
        command: vec![],
    };
    let mut i = 0;
    while i < args.len() {
        let name = args[i].to_string_lossy();
        match name.as_ref() {
            "--note" => o.note = arg(args, &mut i, "--note")?,
            "--file" => o.file = Some(PathBuf::from(arg(args, &mut i, "--file")?)),
            "--metrics" => o.metrics = Some(PathBuf::from(arg(args, &mut i, "--metrics")?)),
            "--reason" => o.reason = arg(args, &mut i, "--reason")?,
            "--seed" => {
                o.seed = arg(args, &mut i, "--seed")?
                    .to_str()
                    .ok_or_else(|| Error::usage("--seed requires an integer"))?
                    .into();
                let v = o.seed.strip_prefix('-').unwrap_or(&o.seed);
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Error::usage("--seed requires an integer"));
                }
            }
            "--group" => {
                o.group = arg(args, &mut i, "--group")?.to_string_lossy().into_owned();
                if !label(&o.group) {
                    return Err(Error::usage(
                        "--group label must match [A-Za-z0-9._-]+ and be at most 60 characters",
                    ));
                }
            }
            "--no-gate" => o.gate = false,
            "-h" | "--help" => {
                return Err(Error {
                    code: 0,
                    message: HELP.into(),
                });
            }
            "--" => {
                o.command = args[i + 1..].to_vec();
                break;
            }
            _ => return Err(Error::usage(format!("unknown argument before --: {name}"))),
        }
        i += 1;
    }
    if o.command.is_empty() {
        return Err(Error::usage(HELP));
    }
    Ok(o)
}
fn git_state(ctx: &Context<'_>) -> Result<(String, usize, String)> {
    let repo = process::git(ctx, &["rev-parse", "--git-dir"], 65536)?;
    if repo.code != 0 {
        if ctx
            .cwd
            .ancestors()
            .any(|p| p.join(".git").symlink_metadata().is_ok())
        {
            return Err(Error::usage("Git state unavailable"));
        }
        return Ok(("none".into(), 0, String::new()));
    }
    let head = process::git(ctx, &["rev-parse", "--verify", "--quiet", "HEAD"], 65536)?;
    let sha = if head.code == 0 {
        let short = process::git(ctx, &["rev-parse", "--short", "HEAD"], 65536)?;
        if short.code != 0 {
            return Err(Error::usage("Git HEAD query failed"));
        }
        String::from_utf8_lossy(&short.bytes)
            .trim_end_matches('\n')
            .into()
    } else if head.code == 1 {
        "no-commit".into()
    } else {
        return Err(Error::usage("Git HEAD state unknown"));
    };
    let status = process::git(
        ctx,
        &["status", "--porcelain", "--untracked-files=no"],
        16 * 1024 * 1024,
    )?;
    if status.code != 0 {
        return Err(Error::usage("Git status query failed"));
    }
    let dirty = status.bytes.iter().filter(|b| **b == b'\n').count();
    let diff = if dirty == 0 {
        String::new()
    } else if sha != "no-commit" {
        process::git_hash(ctx, &["diff", "HEAD"])?[..16].into()
    } else {
        let mut h = sha2::Sha256::new();
        use sha2::Digest;
        // Hash the concatenated two native Git streams, matching the unborn-HEAD contract.
        let mut sink = HashWriter(&mut h);
        for args in [["diff", "--cached"].as_slice(), ["diff"].as_slice()] {
            let argv = std::iter::once(OsString::from("git"))
                .chain(args.iter().map(OsString::from))
                .collect::<Vec<_>>();
            let r = process::execute(
                ctx,
                &argv,
                false,
                0,
                Some(ctx.query_timeout),
                &mut sink,
                &mut std::io::sink(),
            )?;
            if r.code != 0 {
                return Err(Error::usage("Git diff query failed"));
            }
        }
        format!("{:x}", h.finalize())[..16].into()
    };
    Ok((sha, dirty, diff))
}
struct HashWriter<'a>(&'a mut sha2::Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn research(ctx: &Context<'_>) -> Result<Option<Json>> {
    let Some(path) = ctx
        .variable("OMS_RESEARCH_METADATA_FILE")
        .filter(|p| !p.is_empty())
    else {
        return Ok(None);
    };
    let path = store::absolute(&ctx.cwd, Path::new(&path));
    let Ok(bytes) = store::safe_read_in(&path, 16384, false, &ctx.cwd) else {
        return Ok(None);
    };
    let Ok(v) = parse(&bytes) else {
        return Ok(None);
    };
    let mut out = Json::Object(vec![]);
    for key in [
        "question",
        "hypothesis",
        "prediction",
        "baseline",
        "metric",
        "success",
        "change",
    ] {
        if let Some(v) = get(&v, key).filter(|v| !matches!(v, Json::Null) && text(v) != Some("")) {
            put(
                &mut out,
                key,
                string(display(v).chars().take(1000).collect::<String>()),
            );
        }
    }
    let d = s(&v, "contract_digest");
    if d.len() == 64
        && d.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        put(&mut out, "contract_digest", string(d));
    }
    if matches!(&out,Json::Object(o) if !o.is_empty()) {
        Ok(Some(out))
    } else {
        Ok(None)
    }
}
fn gate(
    ctx: &Context<'_>,
    o: &Options,
    override_code: Option<i32>,
    err: &mut dyn Write,
) -> Result<&'static str> {
    if override_code.is_some() {
        return Ok("recorded");
    }
    let check = ctx.cwd.join("scripts/check.sh");
    if !std::fs::metadata(&check).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    {
        return Ok("none");
    }
    let explicit = ctx
        .variable("OMS_RUN_LEDGER_CHECK_MODE")
        .filter(|v| !v.is_empty());
    if o.command.len() >= 3
        && o.command[0] == "bash"
        && o.command[1] == "scripts/check.sh"
        && (o.command[2] == "fast" || o.command[2] == "ml-smoke")
        && explicit.as_ref().is_none_or(|x| *x == o.command[2])
    {
        return Ok("recorded");
    }
    if !o.gate {
        if o.reason.is_empty() {
            return Err(Error::usage(
                "skipping the applicable pre-flight gate requires --reason (or OMS_RUN_LEDGER_GATE_REASON); the override is recorded in the ledger",
            ));
        }
        if sensitive(ctx, o.reason.as_bytes())? {
            return Err(Error::usage(
                "gate skip --reason looks sensitive; use a clean audit reason (it is recorded in the git-tracked ledger)",
            ));
        }
        writeln!(
            err,
            "warning: UNSAFE pre-flight gate skip recorded (reason in ledger)"
        )?;
        return Ok("skipped");
    }
    let mut mode = explicit
        .as_ref()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|| "fast".into());
    if !["fast", "ml-smoke"].contains(&mode.as_str()) {
        return Err(Error::usage(
            "OMS_RUN_LEDGER_CHECK_MODE must be fast or ml-smoke",
        ));
    }
    if explicit.as_ref().is_none_or(|x| x.is_empty()) {
        let bytes = store::safe_read_in(&check, 4 * 1024 * 1024, false, &ctx.cwd)?;
        let text = String::from_utf8_lossy(&bytes);
        let has = |word: &str| {
            regex::Regex::new(&format!(r#"(^|[\s("'|]){}["']?\)"#, regex::escape(word)))
                .unwrap()
                .is_match(&text)
        };
        if !has("fast") && has("ml-smoke") {
            mode = "ml-smoke".into();
        }
    }
    writeln!(
        err,
        "ledger: pre-flight gate: bash scripts/check.sh {mode} (skip with --no-gate --reason ...)"
    )?;
    let result = process::merged(
        ctx,
        &["bash".into(), "scripts/check.sh".into(), mode.into()],
        ctx.command_timeout,
        err,
    )?;
    if result.code != 0 {
        return Err(Error {
            code: if ctx.interrupted() {
                ctx.signal_code()
            } else {
                3
            },
            message: "pre-flight check failed; launch aborted (--no-gate --reason ... to override)"
                .into(),
        });
    }
    Ok("passed")
}

pub fn execute(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExecutionRecord> {
    let o = options(args, ctx)?;
    ctx.check()?;
    let metrics_max = if o.metrics.is_some() {
        let raw = ctx.setting("OMS_METRICS_MAX_BYTES");
        if raw.is_empty() {
            65536
        } else {
            raw.parse::<usize>()
                .map_err(|_| Error::usage("OMS_METRICS_MAX_BYTES must be a non-negative integer"))?
        }
    } else {
        65536
    };
    let override_status = if let Some(v) = ctx
        .variable("OMS_RUN_LEDGER_STATUS_OVERRIDE")
        .filter(|v| !v.is_empty())
    {
        let raw = v.to_str().ok_or_else(|| {
            Error::usage("OMS_RUN_LEDGER_STATUS_OVERRIDE must be a non-negative integer")
        })?;
        if raw.is_empty() || !raw.bytes().all(|c| c.is_ascii_digit()) {
            return Err(Error::usage(
                "OMS_RUN_LEDGER_STATUS_OVERRIDE must be a non-negative integer",
            ));
        }
        let digits = raw.trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        if digits.len() > 4300 {
            return Err(Error::usage(
                "completed status exceeds the integer digit limit",
            ));
        }
        Some(digits.to_owned())
    } else {
        None
    };
    let capture = if let Some(raw) = ctx.variable("OMS_RUN_LEDGER_CAPTURED") {
        if override_status.is_none() {
            return Err(Error::usage(
                "captured metadata is only valid for a completed capsule companion",
            ));
        }
        Some(metrics::decode_capture(raw.as_bytes())?)
    } else {
        None
    };
    let repo = root(ctx, &ctx.cwd)?;
    let ledger = store::absolute(
        &ctx.cwd,
        o.file
            .as_deref()
            .unwrap_or(&repo.join("docs/EXPERIMENTS.jsonl")),
    );
    let contract_root = contract_root(ctx, &ledger)?;
    if journal_enabled(ctx) {
        require(services, Service::Journal, "journal")?;
    }
    let run_id = effective_run(ctx, &repo)?;
    if !run_id.is_empty() {
        require(services, Service::RunLink, "run-link")?;
    }
    let (sha, dirty, diff) = git_state(ctx)?;
    let cmd = Json::Array(o.command.iter().map(|a| os(a)).collect());
    let mut duplicate = None;
    store::rows_in(&ledger, true, &repo, |row| {
        if get(&row, "cmd") == Some(&cmd)
            && s(&row, "git_sha") == sha
            && s(&row, "dirty_hash") == diff
        {
            duplicate = Some(row);
        }
        Ok(())
    })?;
    if let Some(row) = duplicate {
        let result = if get(&row, "exit").is_none_or(|v| matches!(v, Json::Null)) {
            "started (unfinished)".into()
        } else {
            format!("exit={} {}s", s(&row, "exit"), s(&row, "duration_s"))
        };
        let note = s(&row, "note");
        writeln!(
            err,
            "warning: identical run already in ledger (same commit, diff, command): {} {result}{}",
            s(&row, "ts"),
            if note.is_empty() {
                String::new()
            } else {
                format!(" note={note}")
            }
        )?;
    }
    let gate = gate(ctx, &o, override_status.as_ref().map(|_| 0), err)?;
    let mut audit = o.note.as_bytes().to_vec();
    for a in &o.command {
        audit.push(b'\n');
        audit.extend(a.as_bytes());
    }
    if sensitive(ctx, &audit)? {
        writeln!(
            err,
            "warning: ledger note/command looks sensitive; it is recorded in git-tracked {}",
            ledger.display()
        )?;
    }
    let mut random = [0u8; 16];
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    }
    let row_id = format!(
        "ledger-{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let submission = Path::new(&o.command[0]).file_name() == Some(OsStr::new("sbatch"));
    let mut row = object(vec![
        ("schema", num(1)),
        ("id", string(row_id)),
        ("ts", string(now())),
        ("git_sha", string(&sha)),
        ("dirty", num(dirty)),
        ("dirty_hash", string(&diff)),
        (
            "slurm_job_id",
            string(if submission {
                String::new()
            } else {
                ctx.setting("SLURM_JOB_ID")
            }),
        ),
        ("exit", Json::Null),
        ("duration_s", Json::Null),
        ("note", os(&o.note)),
        ("cmd", cmd),
        ("cwd", os(ctx.cwd.as_os_str())),
        ("gate", string(gate)),
    ]);
    if gate == "skipped" {
        put(&mut row, "gate_reason", os(&o.reason));
    }
    let operation = ctx.setting("OMS_OPERATION_ID");
    if !operation.is_empty() {
        put(&mut row, "operation_id", string(operation));
    }
    if !run_id.is_empty() {
        put(&mut row, "run_id", string(&run_id));
    }
    let (seed, group) =
        metrics::seed_and_group(get(&row, "cmd").unwrap(), &sha, &diff, &o.seed, &o.group)?;
    if let Some(seed) = seed {
        put(&mut row, "seed", seed);
    }
    if let Some(group) = group {
        put(&mut row, "group", string(group));
    }
    if let Some(metadata) = research(ctx)? {
        put(&mut row, "research", metadata);
    }
    if ctx
        .variable("OMS_RESEARCH_METADATA_FILE")
        .is_some_and(|x| !x.is_empty())
        && override_status.is_none()
    {
        put(&mut row, "status", string("started"));
        store::append(ctx, &ledger, &row)?;
        remove(&mut row, "status");
    }
    let started = SystemTime::now();
    let result = if let Some(status) = &override_status {
        let code = status.parse::<i64>().map(|n| (n % 256) as i32).unwrap_or(2);
        process::Output {
            code,
            bytes: vec![],
            stderr: vec![],
            hash: String::new(),
            submitted: String::new(),
        }
    } else {
        process::execute(ctx, &o.command, false, 0, ctx.command_timeout, out, err)?
    };
    let code = result.code;
    put(
        &mut row,
        "exit",
        override_status
            .map(Json::Number)
            .unwrap_or_else(|| num(code)),
    );
    put(
        &mut row,
        "duration_s",
        num(started.elapsed().unwrap_or_default().as_secs()),
    );
    if submission {
        put(&mut row, "slurm_job_id", string(result.submitted));
    }
    if let Some(path) = o.metrics {
        let path = store::absolute(&ctx.cwd, &path);
        match store::safe_read_in(&path, metrics_max, false, &ctx.cwd)
            .and_then(|b| metrics::sanitize(&b))
        {
            Ok(Some(v)) => {
                if sensitive(ctx, &encode(&v, false, true)?)? {
                    writeln!(
                        err,
                        "ledger: metrics omitted because they contain sensitive-looking content"
                    )?;
                } else {
                    put(&mut row, "metrics", v);
                }
            }
            Ok(None) => (),
            Err(e) if e.message.contains("InputTooLarge") => writeln!(
                err,
                "ledger: metrics file exceeds {metrics_max}B; row recorded without metrics"
            )?,
            Err(e) if e.message.contains("Missing") => writeln!(
                err,
                "ledger: metrics file not found; row recorded without metrics"
            )?,
            Err(e) => {
                writeln!(
                    err,
                    "ledger: metrics file unreadable ({}); row recorded without metrics",
                    e.message
                )?;
            }
        }
    }
    if let Some(capture) = capture {
        metrics::attach_capture(&mut row, &capture)?;
    } else {
        match metrics::row_contract(&ledger, &contract_root) {
            Ok(Some(c)) => put(&mut row, "metric_contract", c),
            Ok(None) => (),
            Err(e) => writeln!(
                err,
                "ledger: metric contract ignored ({}); row recorded without it",
                e.message
            )?,
        }
    }
    // Cancellation has ended/reaped the child; the final audit record must still be durable.
    store::append_final(ctx, &ledger, &row)?;
    if !run_id.is_empty() {
        let _ = services.link(RunLink {
            repo: &repo,
            run_id: &run_id,
            record_path: &ledger,
            code,
        });
    }
    observe(ctx, services, &repo, &ledger, &row, "run-ledger", err)?;
    writeln!(
        err,
        "ledger: appended to {} (exit {code}, {}s)",
        ledger.display(),
        s(&row, "duration_s")
    )?;
    Ok(ExecutionRecord { row, ledger, code })
}

fn list(
    args: &[OsString],
    ctx: &Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let json = args.first().is_some_and(|a| a == "--json");
    let args = if json { &args[1..] } else { args };
    if args.len() > 1 {
        return Err(Error::usage("list takes at most N"));
    }
    let n = args
        .first()
        .map(|v| natural(v, false))
        .transpose()?
        .unwrap_or(10);
    let path = root(ctx, &ctx.cwd)?.join("docs/EXPERIMENTS.jsonl");
    let mut last = VecDeque::new();
    let mut total = 0;
    store::lines_in(&path, false, &ctx.cwd, |line| {
        if last.len() == n {
            let old: Vec<u8> = last.pop_front().unwrap();
            total -= old.len();
        }
        if total + line.len() > store::LEDGER_BYTES {
            return Err(Error::usage("requested ledger tail exceeds its byte limit"));
        }
        total += line.len();
        last.push_back(line.to_vec());
        Ok(())
    })?;
    let mut rows = vec![];
    let mut bad = 0;
    for line in last {
        let Ok(row) = parse(&line) else {
            bad += 1;
            continue;
        };
        if !matches!(row, Json::Object(_)) {
            bad += 1;
            continue;
        }
        if json {
            rows.push(row);
            continue;
        }
        let dirty = if get(&row, "dirty").is_some_and(truth) {
            "+dirty"
        } else {
            ""
        };
        let result = if get(&row, "exit").is_none_or(|v| matches!(v, Json::Null)) {
            "started".into()
        } else {
            format!("exit={}  {}s", s(&row, "exit"), s(&row, "duration_s"))
        };
        let g = s(&row, "gate");
        let gate = if g == "skipped" && !s(&row, "gate_reason").is_empty() {
            format!("  gate=skipped({})", clean(&s(&row, "gate_reason"), 60))
        } else if !g.is_empty() {
            format!("  gate={g}")
        } else {
            String::new()
        };
        let command = if let Some(Json::Array(words)) = get(&row, "cmd") {
            words.iter().map(display).collect::<Vec<_>>().join(" ")
        } else {
            String::new()
        };
        let mut metric = vec![];
        if let Some(Json::Object(m)) = get(&row, "metrics") {
            for (k, v) in m {
                metric.push(format!(
                    "{}={}",
                    clean(&String::from_utf8_lossy(k), 60),
                    clean(&display(v), 60)
                ));
            }
        }
        let note = s(&row, "note");
        writeln!(
            out,
            "{}  {result}{gate}  sha={}{dirty}  {command}{}{}",
            s(&row, "ts"),
            s(&row, "git_sha"),
            if metric.is_empty() {
                String::new()
            } else {
                format!("  [{}]", metric.join(" "))
            },
            if note.is_empty() {
                String::new()
            } else {
                format!("  # {note}")
            }
        )?;
    }
    if json {
        out.write_all(&encode(
            &object(vec![("schema", num(1)), ("runs", Json::Array(rows))]),
            false,
            true,
        )?)?;
        writeln!(out)?;
    }
    if bad > 0 {
        writeln!(
            err,
            "warning: run ledger contains {bad} invalid row(s) in the last {n} lines; valid rows still shown"
        )?;
    }
    Ok(0)
}
struct RankOptions {
    metric: String,
    direction: String,
    all: bool,
    mixed: bool,
    by_group: bool,
    file: Option<PathBuf>,
    n: usize,
    format: String,
}
fn rank_options(args: &[OsString], table: bool) -> Result<RankOptions> {
    let mut o = RankOptions {
        metric: String::new(),
        direction: String::new(),
        all: false,
        mixed: false,
        by_group: table,
        file: None,
        n: 10,
        format: "md".into(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_string_lossy();
        match a.as_ref() {
            "--metric" => {
                o.metric = arg(args, &mut i, "--metric")?
                    .to_string_lossy()
                    .into_owned()
            }
            "--min" => o.direction = "min".into(),
            "--max" => o.direction = "max".into(),
            "--all" => o.all = true,
            "--mixed" => o.mixed = true,
            "--by-group" if !table => o.by_group = true,
            "--file" => o.file = Some(PathBuf::from(arg(args, &mut i, "--file")?)),
            "--format" if table => {
                o.format = arg(args, &mut i, "--format")?
                    .to_string_lossy()
                    .into_owned()
            }
            "-h" | "--help" => {
                return Err(Error {
                    code: 0,
                    message: HELP.into(),
                });
            }
            _ if !table => o.n = natural(&args[i], false)?,
            _ => return Err(Error::usage(format!("unknown table argument: {a}"))),
        }
        i += 1;
    }
    if !["md", "tsv", "tex"].contains(&o.format.as_str()) {
        return Err(Error::usage("--format must be md, tsv or tex"));
    }
    if !table && o.metric.is_empty() {
        return Err(Error::usage("top requires --metric KEY"));
    }
    Ok(o)
}
fn detail(left: &std::collections::BTreeMap<String, usize>) -> String {
    left.iter()
        .map(|(k, n)| format!("{n} run(s): {k}"))
        .collect::<Vec<_>>()
        .join("; ")
}
fn rank_one(
    o: &RankOptions,
    path: &Path,
    contract_root: &Path,
    table: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let mode = if table { "table" } else { "top" };
    let declared = metrics::load_in(path, contract_root).map_err(|e| {
        Error::usage(format!(
            "metric contract unreadable ({}); fix it or move it aside",
            e.message
        ))
    })?;
    let spec = get(&declared, &o.metric);
    let current = spec
        .map(|s| metrics::record(s, contract_root))
        .transpose()?;
    let direction = if o.direction.is_empty() {
        spec.map(|spec| s(spec, "direction"))
            .unwrap_or_else(|| "max".into())
    } else {
        o.direction.clone()
    };
    if let Some(spec) = spec
        && !o.direction.is_empty()
        && s(spec, "direction") != o.direction
    {
        writeln!(
            err,
            "warning: the metric contract declares {} for {}; ranking --{} as asked",
            s(spec, "direction"),
            o.metric,
            o.direction
        )?;
    }
    let mut rows = vec![];
    let mut row_bytes = 0;
    store::rows_in(path, false, contract_root, |row| {
        let v = get(&row, "metrics")
            .and_then(|m| get(m, &o.metric))
            .and_then(number);
        if let Some(v) = v
            && (o.all || get(&row, "exit").and_then(number) == Some(0.0))
        {
            row_bytes += encode(&row, false, false)?.len();
            if row_bytes > store::LEDGER_BYTES {
                return Err(Error::usage("ranking input exceeds its byte limit"));
            }
            rows.push((v, row));
        }
        Ok(())
    })?;
    if rows.is_empty() {
        writeln!(
            err,
            "{mode}: no ledger rows with a finite numeric metric {:?}{}",
            o.metric,
            if o.all {
                ""
            } else {
                " and exit==0 (use --all to include failed runs)"
            }
        )?;
        return Ok(3);
    }
    if o.mixed {
        let digests = rows
            .iter()
            .map(|(_, r)| metrics::row_digest(r, &o.metric))
            .collect::<BTreeSet<_>>();
        if current.is_some() || digests.len() > 1 {
            writeln!(
                err,
                "warning: --mixed {} runs measured under different metric contracts together",
                if table { "tabulates" } else { "ranks" }
            )?;
        }
    } else {
        let (kept, left) = metrics::comparable(rows, &o.metric, current.as_ref());
        rows = kept;
        let details = detail(&left);
        if rows.is_empty() {
            writeln!(
                err,
                "{mode}: not comparable: {} runs do not share one metric contract ({details}); --mixed {} them anyway",
                o.metric,
                if table { "tabulates" } else { "ranks" }
            )?;
            return Ok(4);
        }
        if !left.is_empty() {
            writeln!(
                err,
                "{mode}: left out as not comparable under today's contract for {}: {details}",
                o.metric
            )?;
        }
    }
    // Tables and top share the documented data filter, including known/unrecorded separation.
    let (data, left) = metrics::data_comparable(rows.iter().map(|(v, r)| (*v, copy(r))).collect());
    if !left.is_empty() {
        if o.mixed {
            writeln!(
                err,
                "warning: --mixed ignores recorded data comparability ({})",
                detail(&left)
            )?;
        } else if data.is_empty() {
            writeln!(
                err,
                "{mode}: not comparable: recorded data identities disagree or are invalid ({}); --mixed overrides this ranking check",
                detail(&left)
            )?;
            return Ok(4);
        } else {
            writeln!(
                err,
                "{mode}: excluded invalid recorded input identities ({})",
                detail(&left)
            )?;
        }
    }
    if !o.mixed {
        rows = data;
    }
    if o.by_group {
        let (mut groups, loose) = metrics::groups(&rows, &o.metric)?;
        if groups.is_empty() {
            writeln!(
                err,
                "{mode}: {}no run with a seed and group for {} (record with --seed N, or a command carrying --seed N)",
                if table { "" } else { "--by-group found " },
                o.metric
            )?;
            return Ok(3);
        }
        groups.sort_by(|a, b| {
            if direction == "max" {
                b.mean.total_cmp(&a.mean)
            } else {
                a.mean.total_cmp(&b.mean)
            }
        });
        if table {
            render_table(&o.metric, &direction, &groups, &o.format, out)?;
        } else {
            for (i, g) in groups.iter().filter(|g| g.n > 1).take(o.n).enumerate() {
                let ci = g.ci.unwrap();
                writeln!(
                    out,
                    "{:2}  {} mean={}  stdev={}  ci95=[{}, {}]  n={}  group={}  seeds={}",
                    i + 1,
                    o.metric,
                    significant(g.mean, 6, false),
                    significant(g.stdev.unwrap(), 6, false),
                    significant(ci[0], 6, false),
                    significant(ci[1], 6, false),
                    g.n,
                    clean(&g.label, 60),
                    g.seeds.join(",")
                )?;
            }
            if groups.iter().any(|g| g.n == 1) {
                writeln!(out, "single seed (no claim that one is better):")?;
                for g in groups.iter().filter(|g| g.n == 1).take(o.n) {
                    writeln!(
                        out,
                        "    {}={}  n=1  group={}  seed={}",
                        o.metric,
                        significant(g.mean, 6, false),
                        clean(&g.label, 60),
                        g.seeds[0]
                    )?;
                }
            }
        }
        if loose > 0 {
            writeln!(
                err,
                "{mode}: {loose} run(s) without a usable seed group or input identity are not grouped"
            )?;
        }
        return Ok(0);
    }
    rows.sort_by(|a, b| {
        if direction == "max" {
            b.0.total_cmp(&a.0)
        } else {
            a.0.total_cmp(&b.0)
        }
    });
    for (i, (v, row)) in rows.iter().take(o.n).enumerate() {
        let command = if let Some(Json::Array(a)) = get(row, "cmd") {
            a.iter().map(display).collect::<Vec<_>>().join(" ")
        } else {
            String::new()
        };
        let note = s(row, "note");
        writeln!(
            out,
            "{:2}  {}={}  {}{}  sha={}{}  {}{}",
            i + 1,
            o.metric,
            get(row, "metrics")
                .and_then(|m| get(m, &o.metric))
                .map(display)
                .unwrap_or_else(|| v.to_string()),
            s(row, "ts"),
            if o.all {
                format!("  exit={}", s(row, "exit"))
            } else {
                String::new()
            },
            s(row, "git_sha"),
            if get(row, "dirty").is_some_and(truth) {
                "+dirty"
            } else {
                ""
            },
            clean(&command, 60),
            if note.is_empty() {
                String::new()
            } else {
                format!("  # {}", clean(&note, 60))
            }
        )?;
    }
    Ok(0)
}
pub fn significant(value: f64, digits: usize, zeros: bool) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let scientific = format!("{:.*e}", digits - 1, value);
    let (mantissa, power) = scientific.split_once('e').unwrap();
    let power = power.parse::<i32>().unwrap();
    let mut s = if power < -4 || power >= digits as i32 {
        format!("{mantissa}e{power}")
    } else {
        format!("{:.*}", (digits as i32 - power - 1).max(0) as usize, value)
    };
    if !zeros {
        if let Some((m, e)) = s.split_once('e') {
            let m = m.trim_end_matches('0').trim_end_matches('.');
            s = format!("{m}e{:+03}", e.parse::<i32>().unwrap());
        } else if s.contains('.') {
            s = s.trim_end_matches('0').trim_end_matches('.').into();
        }
    } else if !s.contains(['.', 'e']) {
        s.push('.');
    }
    s
}
fn tex_text(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\\' => "\\textbackslash{}".into(),
            '_' => "\\_".into(),
            '%' => "\\%".into(),
            '&' => "\\&".into(),
            '#' => "\\#".into(),
            '$' => "\\$".into(),
            '{' => "\\{".into(),
            '}' => "\\}".into(),
            '~' => "\\textasciitilde{}".into(),
            '^' => "\\textasciicircum{}".into(),
            _ => c.to_string(),
        })
        .collect()
}
fn render_table(
    metric: &str,
    direction: &str,
    groups: &[metrics::Group],
    format: &str,
    out: &mut dyn Write,
) -> Result<()> {
    let replicated = groups.iter().filter(|g| g.n > 1).collect::<Vec<_>>();
    let singles = groups.iter().filter(|g| g.n == 1);
    let comment = format!(
        "{metric}: mean ± stdev over seeds, ci95 = bootstrap 95% interval, every number rounded to 4 significant digits; ranked {} first; one-seed groups are listed last, unranked and never marked best",
        if direction == "max" {
            "highest"
        } else {
            "lowest"
        }
    );
    let header = [
        "#".into(),
        "group".into(),
        format!("{metric} (mean ± stdev)"),
        "ci95".into(),
        "n".into(),
        "note".into(),
    ];
    match format {
        "md" => {
            writeln!(out, "<!-- {comment} -->")?;
            writeln!(
                out,
                "| {} |",
                header
                    .iter()
                    .map(|s| s.replace('|', "\\|"))
                    .collect::<Vec<_>>()
                    .join(" | ")
            )?;
            writeln!(out, "|---:|---|---|---|---:|---|")?;
        }
        "tsv" => {
            writeln!(out, "# {comment}")?;
            writeln!(out, "{}", header.join("\t"))?;
        }
        _ => {
            writeln!(out, "% {comment}\n\\begin{{tabular}}{{rlllrl}}\n\\hline")?;
            writeln!(
                out,
                "\\# & group & {} (mean $\\pm$ stdev) & ci95 & $n$ & note \\\\\n\\hline",
                tex_text(metric)
            )?;
        }
    }
    let n = |v: f64| {
        let s = significant(v, 4, true);
        if format == "tex"
            && let Some((m, e)) = s.split_once('e')
        {
            return format!("{m}\\times10^{{{e}}}");
        }
        s
    };
    for (i, g) in replicated.iter().copied().chain(singles).enumerate() {
        let mut value = if g.n == 1 {
            n(g.mean)
        } else {
            format!("{} ± {}", n(g.mean), n(g.stdev.unwrap()))
        };
        let mut interval =
            g.ci.map(|ci| format!("[{}, {}]", n(ci[0]), n(ci[1])))
                .unwrap_or_else(|| "-".into());
        let best = i == 0 && replicated.len() > 1;
        let mut name = clean(&g.label, 60);
        if format == "tex" {
            name = tex_text(&name);
            if value.contains('±') || value.contains("times") {
                value = format!("${}$", value.replace('±', "\\pm"));
            }
            if interval.contains("times") {
                interval = format!("${interval}$");
            }
            if best {
                value = format!("\\textbf{{{value}}}");
            }
        } else if format == "md" {
            name = name.replace('|', "\\|");
            if best {
                value = format!("**{value}**");
            }
        }
        let cells = [
            if g.n == 1 {
                "-".into()
            } else {
                (i + 1).to_string()
            },
            name,
            value,
            interval,
            g.n.to_string(),
            if g.n == 1 {
                "single seed".into()
            } else {
                String::new()
            },
        ];
        match format {
            "md" => writeln!(out, "| {} |", cells.join(" | "))?,
            "tsv" => writeln!(out, "{}", cells.join("\t"))?,
            _ => writeln!(out, r"{} \\", cells.join(" & "))?,
        }
    }
    if format == "tex" {
        writeln!(out, "\\hline\n\\end{{tabular}}")?;
    }
    Ok(())
}
pub fn run(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let mode = args.first().and_then(|s| s.to_str()).unwrap_or("");
    match mode {
        "list" => list(&args[1..], ctx, out, err),
        "top" | "table" => {
            let table = mode == "table";
            let mut options = rank_options(&args[1..], table)?;
            let repo = root(ctx, &ctx.cwd)?;
            let path = store::absolute(
                &ctx.cwd,
                options
                    .file
                    .as_deref()
                    .unwrap_or(&repo.join("docs/EXPERIMENTS.jsonl")),
            );
            let contract_root = contract_root(ctx, &path)?;
            if !table {
                match metrics::final_split_report_in(&path, &contract_root) {
                    Ok(notes) => {
                        for note in notes {
                            writeln!(err, "top: {note}")?;
                        }
                    }
                    Err(e) => writeln!(err, "warning: split roles ignored ({})", e.message)?,
                }
            }
            if table && options.metric.is_empty() {
                let contract = metrics::load_in(&path, &contract_root)?;
                let Json::Object(items) = &contract else {
                    unreachable!()
                };
                if items.is_empty() {
                    return Err(Error::usage(
                        "table requires --metric KEY (no metric contract declares any)",
                    ));
                }
                let mut names = items
                    .iter()
                    .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                    .collect::<Vec<_>>();
                names.sort();
                let mut printed = 0;
                for name in names {
                    if printed > 0 {
                        writeln!(out)?;
                    }
                    options.metric = name;
                    if rank_one(&options, &path, &contract_root, true, out, err)? == 0 {
                        printed += 1;
                    }
                }
                Ok(if printed > 0 { 0 } else { 3 })
            } else {
                rank_one(&options, &path, &contract_root, table, out, err)
            }
        }
        _ => Ok(execute(args, ctx, services, out, err)?.code),
    }
}
