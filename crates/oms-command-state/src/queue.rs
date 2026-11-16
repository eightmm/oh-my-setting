//! Task-spooler adapter. The queue owns child lifetime; wait only records its result.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, error, helpers, json, require, storage, success,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const HELP: &str = "Usage: oms tsp-queue enqueue [--label TEXT] [--slots N] [--ledger-note TEXT] [--allow-noqueue] -- COMMAND...\n       oms tsp-queue list\n       oms tsp-queue cancel ID|--all\n       oms tsp-queue wait [ID]\n       oms tsp-queue logs [--full] ID\nA harness child cannot enqueue or cancel host jobs.\n";

fn state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("OMS_TSP_STATE_DIR").filter(|v| !v.is_empty()) {
        return Ok(path.into());
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
        .ok_or_else(|| error(2, "no queue state base"))?;
    Ok(base.join("oh-my-setting/tsp-queue"))
}

fn root(path: &Path) -> Result<PathBuf> {
    let mut at = path;
    while !at.exists() {
        at = at
            .parent()
            .ok_or_else(|| error(2, "queue state has no existing ancestor"))?;
    }
    at.canonicalize().map_err(|e| error(2, e))
}

fn id(raw: &str) -> Result<&str> {
    require(
        !raw.is_empty() && raw.len() <= 20 && raw.bytes().all(|b| b.is_ascii_digit()),
        "job id must be numeric",
    )?;
    Ok(raw)
}

fn invoke(
    ctx: &Context<'_>,
    args: &[OsString],
    query: bool,
    timeout: Option<Duration>,
) -> Result<oms_command_runs::process::Output> {
    let native =
        oms_command_runs::Context::new(&ctx.cwd, ctx.interrupted).map_err(|e| error(e.code, e))?;
    let mut command = vec![OsString::from("tsp")];
    command.extend_from_slice(args);
    oms_command_runs::process::execute(
        &native,
        &command,
        query,
        storage::MAX_FILE,
        timeout,
        &mut std::io::sink(),
        &mut std::io::sink(),
    )
    .map_err(|e| error(e.code, e))
}

pub(crate) fn run(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &mut dyn oms_command_runs::Services,
) -> Result<Output> {
    if args.first().is_some_and(|s| s == "--help" || s == "-h") {
        return Ok(success(HELP));
    }
    let action = args
        .first()
        .and_then(|s| s.to_str())
        .ok_or_else(|| error(2, HELP))?;
    let split = args.iter().position(|s| s == "--");
    let opts = Options::parse(
        &args[1..split.unwrap_or(args.len())],
        &["label", "slots", "ledger-note"],
        &["allow-noqueue", "all", "full"],
    )?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    require(
        ["enqueue", "list", "cancel", "wait", "logs"].contains(&action),
        HELP,
    )?;
    if ["enqueue", "cancel"].contains(&action) {
        crate::parent_only()?;
    }
    let dir = state_dir()?;
    let dir = if dir.is_absolute() {
        dir
    } else {
        ctx.cwd.join(dir)
    };
    let anchor = root(&dir)?;
    if action != "enqueue"
        && (std::env::var("OMS_TSP_FORCE_FALLBACK").unwrap_or_default() == "1"
            || invoke(ctx, &[], true, Some(Duration::from_secs(30))).is_ok_and(|o| o.code == 127))
    {
        return fallback(ctx, &opts, action, services);
    }
    match action {
        "list" => {
            require(
                opts.pos.is_empty() && split.is_none(),
                "list takes no arguments",
            )?;
            let output = invoke(ctx, &[], true, Some(Duration::from_secs(30)))?;
            Ok((output.code, output.bytes, output.stderr))
        }
        "enqueue" => {
            require(opts.pos.is_empty(), "enqueue takes options before --")?;
            let command = split
                .map(|i| &args[i + 1..])
                .filter(|a| !a.is_empty())
                .ok_or_else(|| error(2, "enqueue requires a command after --"))?;
            let slots = opts.number("slots", 1)?;
            require(slots > 0, "--slots requires a positive integer")?;
            if !(oms_scrub::scan_secret(
                &json::canonical(&object([
                    (
                        "command",
                        json::list(crate::strings(command)?.iter().map(String::as_str)),
                    ),
                    ("note", string(opts.get("ledger-note"))),
                ]))?,
                &[],
            )
            .map_err(|e| error(2, format!("{e:?}")))?
            .is_empty())
            {
                return Err(error(3, "command or note looks sensitive"));
            }
            let _guard = storage::lock(&dir.join("last"))?;
            let slots_result = invoke(
                ctx,
                &["-S".into(), slots.to_string().into()],
                true,
                Some(Duration::from_secs(30)),
            )?;
            if slots_result.code != 0
                || std::env::var("OMS_TSP_FORCE_FALLBACK").unwrap_or_default() == "1"
            {
                require(
                    opts.flag("allow-noqueue")
                        || std::env::var("OMS_TSP_ALLOW_FALLBACK").unwrap_or_default() == "1",
                    "tsp is unavailable; pass --allow-noqueue for unqueued execution",
                )?;
                return fallback_enqueue(ctx, &opts, command);
            }
            let mut argv = Vec::new();
            if !opts.get("label").is_empty() {
                argv.extend(["-L".into(), opts.get("label").into()]);
            }
            argv.push("--".into());
            argv.extend_from_slice(command);
            let output = invoke(ctx, &argv, true, Some(Duration::from_secs(30)))?;
            require(
                output.code == 0,
                format!("tsp enqueue failed (exit {})", output.code),
            )?;
            let job = std::str::from_utf8(&output.bytes)
                .map_err(|_| error(2, "invalid tsp id"))?
                .trim();
            id(job)?;
            let mut row = object([
                ("schema", number(1)),
                ("id", string(job)),
                ("project", string(ctx.repo.to_string_lossy())),
                ("label", string(opts.get("label"))),
                ("note", string(opts.get("ledger-note"))),
                (
                    "command",
                    json::list(crate::strings(command)?.iter().map(String::as_str)),
                ),
            ]);
            let socket = std::env::var("TS_SOCKET").unwrap_or_else(|_| {
                format!(
                    "{}/socket-ts.{}",
                    std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()),
                    rustix::process::getuid().as_raw()
                )
            });
            row.set("status", string(""))?;
            row.set("queue_socket", string(socket))?;
            row.set(
                "queue_command",
                string(format!(
                    "{}{}",
                    if opts.get("label").is_empty() {
                        String::new()
                    } else {
                        format!("[{}]", opts.get("label"))
                    },
                    crate::strings(command)?.join(" ")
                )),
            )?;
            // The queue may own the job even if metadata publication fails: never resubmit blindly.
            storage::replace_locked(&anchor, &dir.join(job), &metadata(&row, false)?).map_err(
                |e| {
                    error(
                        1,
                        format!(
                            "job {job} submitted; metadata failed: {e}; reconcile before retry"
                        ),
                    )
                },
            )?;
            storage::replace_locked(&anchor, &dir.join("last"), format!("{job}\n").as_bytes())?;
            Ok(success(format!("{job}\n")))
        }
        "cancel" => {
            require(
                (opts.flag("all") && opts.pos.is_empty())
                    || (!opts.flag("all") && opts.pos.len() == 1),
                "cancel requires ID or --all",
            )?;
            let jobs = if opts.flag("all") {
                let output = invoke(ctx, &[], true, Some(Duration::from_secs(30)))?;
                require(output.code == 0, "tsp listing failed")?;
                String::from_utf8_lossy(&output.bytes)
                    .lines()
                    .filter_map(|l| l.split_whitespace().next())
                    .filter(|s| id(s).is_ok())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            } else {
                vec![id(&opts.pos[0])?.to_owned()]
            };
            for job in jobs {
                let _ = invoke(
                    ctx,
                    &["-k".into(), job.clone().into()],
                    true,
                    Some(Duration::from_secs(30)),
                )?;
                let result = invoke(
                    ctx,
                    &["-r".into(), job.into()],
                    true,
                    Some(Duration::from_secs(30)),
                )?;
                if !opts.flag("all") {
                    require(result.code == 0, "tsp remove failed")?;
                }
            }
            if opts.flag("all") {
                let result = invoke(ctx, &["-C".into()], true, Some(Duration::from_secs(30)))?;
                require(result.code == 0, "tsp clear failed")?;
            }
            Ok(success(""))
        }
        "wait" => {
            require(opts.pos.len() <= 1, "wait takes at most one id")?;
            let job = if let Some(job) = opts.pos.first() {
                job.clone()
            } else {
                String::from_utf8(storage::read(&anchor, &dir.join("last"), 128, false)?)
                    .map_err(|_| error(2, "invalid last job"))?
                    .trim()
                    .into()
            };
            id(&job)?;
            let waited = invoke(ctx, &["-w".into(), job.clone().into()], true, None)?;
            if waited.code >= 129 {
                return Ok((waited.code, waited.bytes, waited.stderr));
            }
            let status = invoke(
                ctx,
                &["-s".into(), job.clone().into()],
                true,
                Some(Duration::from_secs(30)),
            )?;
            require(
                status.code == 0,
                "job status is unknown; ledger not recorded",
            )?;
            let status = String::from_utf8_lossy(&status.bytes);
            let parsed = regex::Regex::new(r"(?i)exit[^0-9]*([0-9]+)")
                .expect("static expression")
                .captures(&status)
                .map(|m| m[1].to_owned())
                .or_else(|| status.trim().parse::<u32>().ok().map(|v| v.to_string()))
                .or_else(|| {
                    ["success", "finished", "done"]
                        .iter()
                        .any(|s| status.to_lowercase().contains(s))
                        .then(|| "0".into())
                        .or_else(|| {
                            ["error", "died", "fail"]
                                .iter()
                                .any(|s| status.to_lowercase().contains(s))
                                .then(|| "1".into())
                        })
                })
                .ok_or_else(|| error(2, "job exit is unknown; ledger not recorded"))?;
            let raw = storage::read(&anchor, &dir.join(&job), storage::MAX_FILE, false)?;
            let row = parse_metadata(&raw)?;
            let mut native =
                oms_command_runs::Context::new(Path::new(row.text("project")), ctx.interrupted)
                    .map_err(|e| error(e.code, e))?;
            native.env.insert(
                "OMS_RUN_LEDGER_STATUS_OVERRIDE".into(),
                parsed.clone().into(),
            );
            let mut argv: Vec<OsString> = vec![
                "--no-gate".into(),
                "--note".into(),
                row.text("note").into(),
                "--".into(),
            ];
            argv.extend(
                row.get("command")
                    .strings()?
                    .into_iter()
                    .map(OsString::from),
            );
            oms_command_runs::execute(
                &argv,
                &native,
                services,
                &mut std::io::sink(),
                &mut std::io::sink(),
            )
            .map_err(|e| error(e.code, e))?;
            Ok(success(format!("{parsed}\n")))
        }
        "logs" => {
            require(opts.pos.len() == 1, "logs requires one id")?;
            let output = invoke(
                ctx,
                &["-o".into(), id(&opts.pos[0])?.into()],
                true,
                Some(Duration::from_secs(30)),
            )?;
            require(output.code == 0, "tsp log location unavailable")?;
            let path = helpers::absolute(&ctx.cwd, String::from_utf8_lossy(&output.bytes).trim());
            let bytes = log_bytes(&path, opts.flag("full"))?;
            Ok((0, bytes, vec![]))
        }

        _ => Err(error(2, HELP)),
    }
}

fn quoted(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}
fn metadata(row: &Value, fallback: bool) -> Result<Vec<u8>> {
    let text = String::from_utf8(json::canonical(row)?).map_err(|e| error(2, e))?;
    let mut out = format!("# oms-panel {text}\n# oms-native {text}\n");
    let prefix = if fallback { "f" } else { "meta" };
    out.push_str(&format!(
        "{prefix}_note={}\n{prefix}_cmd=( {} )\n",
        quoted(row.text("note")),
        row.get("command")
            .strings()?
            .into_iter()
            .map(quoted)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    if fallback {
        for (field, key) in [
            ("pid", "id"),
            ("log", "log"),
            ("status", "status"),
            ("child", "child"),
        ] {
            out.push_str(&format!("f_{field}={}\n", quoted(row.text(key))));
        }
    }
    Ok(out.into_bytes())
}
fn parse_metadata(raw: &[u8]) -> Result<Value> {
    for line in raw.split(|b| *b == b'\n') {
        if let Some(raw) = line.strip_prefix(b"# oms-native ") {
            return json::parse(raw);
        }
    }
    let text = std::str::from_utf8(raw).map_err(|_| error(2, "queue metadata is not UTF-8"))?;
    let mut row = text
        .lines()
        .find_map(|l| l.strip_prefix("# oms-panel "))
        .map(|l| json::parse(l.as_bytes()))
        .transpose()?
        .unwrap_or_else(json::empty);
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "meta_cmd" | "f_cmd" => {
                    let value = value
                        .trim()
                        .strip_prefix('(')
                        .and_then(|v| v.strip_suffix(')'))
                        .ok_or_else(|| error(2, "malformed stored argv"))?;
                    row.set(
                        "command",
                        json::list(shell_words(value)?.iter().map(String::as_str)),
                    )?;
                }
                "meta_note" | "f_note" => row.set("note", string(shell_words(value)?.join(" ")))?,
                "f_pid" => row.set("id", string(shell_words(value)?.join(" ")))?,
                "f_log" => row.set("log", string(shell_words(value)?.join(" ")))?,
                "f_status" => row.set("status", string(shell_words(value)?.join(" ")))?,
                "f_child" => row.set("child", string(shell_words(value)?.join(" ")))?,
                _ => {}
            }
        }
    }
    require(
        row.get("command").strings().is_ok_and(|a| !a.is_empty())
            && !row.text("project").is_empty(),
        "queue metadata has no project/argv receipt",
    )?;
    Ok(row)
}
fn log_bytes(path: &Path, full: bool) -> Result<Vec<u8>> {
    use std::io::{BufRead, BufReader, Read};
    let mut file = storage::input_file(path)?;
    if full {
        let mut out = Vec::new();
        file.read_to_end(&mut out).map_err(|e| error(2, e))?;
        return Ok(out);
    }
    let maximum = std::env::var("OMS_TSP_LOG_LINES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(200);
    let mut ring = std::collections::VecDeque::new();
    let mut reader = BufReader::new(file);
    loop {
        let mut line = Vec::new();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|e| error(2, e))?
            == 0
        {
            break;
        }
        ring.push_back(line);
        if ring.len() > maximum {
            ring.pop_front();
        }
    }
    Ok(ring.into_iter().flatten().collect())
}
fn fallback_dir(ctx: &Context<'_>) -> Result<PathBuf> {
    Ok(helpers::absolute(
        &ctx.cwd,
        std::env::var_os("OMS_TSP_FALLBACK_DIR")
            .map(PathBuf::from)
            .unwrap_or(
                state_dir()?
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("tsp-fallback"),
            ),
    ))
}
fn fallback_enqueue(ctx: &Context<'_>, opts: &Options, command: &[OsString]) -> Result<Output> {
    use std::os::unix::process::CommandExt;
    let helper = std::env::var_os("OMS_TSP_WORKER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("oms-state-queue-worker")))
        })
        .ok_or_else(|| error(2, "native queue worker is unavailable"))?;
    require(
        helper.is_file() && rustix::fs::access(&helper, rustix::fs::Access::EXEC_OK).is_ok(),
        "unqueued execution requires the installed oms-state-queue-worker executable",
    )?;
    let dir = fallback_dir(ctx)?;
    let request = dir.join(format!("request-{}.json", crate::random_id("")?));
    let anchor = root(&dir)?;
    let mut row = object([
        ("schema", number(1)),
        ("project", string(ctx.repo.to_string_lossy())),
        ("cwd", string(ctx.cwd.to_string_lossy())),
        ("note", string(opts.get("ledger-note"))),
        ("label", string(opts.get("label"))),
        (
            "command",
            json::list(crate::strings(command)?.iter().map(String::as_str)),
        ),
    ]);
    storage::replace_locked(&anchor, &request, &json::encoded(&row)?)?;
    let mut child = std::process::Command::new(helper)
        .arg(&request)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| error(2, e))?;
    let job = child.id().to_string();
    let work = dir.join(&job);
    for (key, value) in [
        ("id", string(&job)),
        ("log", string(work.join("output.log").to_string_lossy())),
        ("status", string(work.join("status").to_string_lossy())),
        ("child", string(work.join("child").to_string_lossy())),
    ] {
        row.set(key, value)?;
    }
    let result = (|| {
        let _guard = storage::lock(&dir.join("last"))?;
        storage::replace_locked(
            &anchor,
            &dir.join(format!("{job}.meta")),
            &metadata(&row, true)?,
        )?;
        storage::replace_locked(&anchor, &dir.join("last"), format!("{job}\n").as_bytes())?;
        storage::replace_locked(&anchor, &work.join("ready.json"), &json::encoded(&row)?)?;
        Ok::<_, crate::Error>(())
    })();
    if let Err(e) = result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok((
        0,
        format!("{job}\n").into_bytes(),
        b"warning: degraded fallback active (no real queue / no slot limiting)\n".to_vec(),
    ))
}
fn fallback(
    ctx: &Context<'_>,
    opts: &Options,
    action: &str,
    _services: &mut dyn oms_command_runs::Services,
) -> Result<Output> {
    let dir = fallback_dir(ctx)?;
    let anchor = root(&dir)?;
    if action == "list" {
        let mut out = String::from("ID\tSTATE\tLOG\n");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_none_or(|s| s != "meta") {
                    continue;
                }
                let row = parse_metadata(&storage::input(&entry.path(), storage::MAX_FILE)?)?;
                let status = storage::read(&anchor, Path::new(row.text("status")), 128, true)?;
                out.push_str(&format!(
                    "{}\t{}\t{}\n",
                    row.text("id"),
                    if status.is_empty() {
                        "running".into()
                    } else {
                        format!("exit {}", String::from_utf8_lossy(&status).trim())
                    },
                    row.text("log")
                ));
            }
        }
        return Ok(success(out));
    }
    if action == "cancel" && opts.flag("all") {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|s| s == "meta") {
                    let job = entry
                        .path()
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let mut one = Options::default();
                    one.pos.push(job);
                    fallback(ctx, &one, "cancel", _services)?;
                }
            }
        }
        return Ok(success(""));
    }
    require(opts.pos.len() <= 1, "action takes one job id")?;
    let job = opts.pos.first().cloned().unwrap_or(
        String::from_utf8(storage::read(&anchor, &dir.join("last"), 128, true)?)
            .map_err(|_| error(2, "invalid last job"))?
            .trim()
            .into(),
    );
    id(&job)?;
    let row = parse_metadata(&storage::input(
        &dir.join(format!("{job}.meta")),
        storage::MAX_FILE,
    )?)?;
    match action {
        "logs" => Ok((
            0,
            log_bytes(Path::new(row.text("log")), opts.flag("full"))?,
            vec![],
        )),
        "cancel" => {
            let path = dir.join(&job).join("cancel-request.json");
            storage::replace_locked(
                &anchor,
                &path,
                &json::encoded(&object([
                    ("schema", number(1)),
                    ("task_id", string(&job)),
                    ("kind", string("mcp-task-cancel-request")),
                ]))?,
            )?;
            Ok(success(""))
        }
        "wait" => loop {
            crate::checkpoint(ctx.interrupted)?;
            let status = storage::read(&anchor, Path::new(row.text("status")), 128, true)?;
            if !status.is_empty() {
                return Ok(success(format!(
                    "{}\n",
                    String::from_utf8_lossy(&status).trim()
                )));
            }
            if row
                .text("id")
                .parse::<i32>()
                .ok()
                .and_then(rustix::process::Pid::from_raw)
                .is_some_and(|pid| rustix::process::test_kill_process(pid).is_err())
            {
                return Err(error(
                    2,
                    "fallback worker exited without a status receipt; reconcile before retry",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        },
        _ => Err(error(2, "unknown fallback action")),
    }
}
/// Dedicated helper entrypoint. The ready receipt fences launch from parent publication.
pub(crate) fn worker(request: &Path) -> Result<i32> {
    let raw = storage::input(request, storage::MAX_FILE)?;
    let input = json::parse(&raw)?;
    let dir = request
        .parent()
        .ok_or_else(|| error(2, "invalid queue request"))?;
    let job = std::process::id().to_string();
    let work = dir.join(&job);
    let anchor = root(dir)?;
    let mut ready = None;
    for _ in 0..300 {
        let bytes = storage::read(&anchor, &work.join("ready.json"), storage::MAX_FILE, true)?;
        if !bytes.is_empty() {
            ready = Some(json::parse(&bytes)?);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let row =
        ready.ok_or_else(|| error(2, "queue ready receipt did not arrive; command not started"))?;
    require(
        row.get("command") == input.get("command") && row.text("id") == job,
        "queue launch identity mismatch",
    )?;
    storage::replace_locked(&anchor, Path::new(row.text("log")), b"")?;
    let mut out = storage::log_file(Path::new(row.text("log")))?;
    let mut err = out.try_clone().map_err(|e| error(2, e))?;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut native = oms_command_runs::Context::new(Path::new(row.text("cwd")), &stop)
        .map_err(|e| error(e.code, e))?;
    let policy = oms_command_runs::process::ProcessPolicy {
        cancel_dir: Some(work.clone()),
        ..Default::default()
    };
    let argv = row
        .get("command")
        .strings()?
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    let result = oms_command_runs::process::execute_with_policy(
        &native, &argv, &policy, false, 0, None, &mut out, &mut err,
    )
    .map_err(|e| error(e.code, e))?;
    out.sync_all().map_err(|e| error(1, e))?;
    let mut services = crate::NativeServices {
        source: Path::new(row.text("project")),
        interrupted: &stop,
    };
    native.env.insert(
        "OMS_RUN_LEDGER_STATUS_OVERRIDE".into(),
        result.code.to_string().into(),
    );
    let mut args = vec![
        "--no-gate".into(),
        "--note".into(),
        row.text("note").into(),
        "--".into(),
    ];
    args.extend(argv);
    let ledger = oms_command_runs::execute(
        &args,
        &native,
        &mut services,
        &mut std::io::sink(),
        &mut std::io::sink(),
    );
    storage::replace_locked(
        &anchor,
        Path::new(row.text("status")),
        format!("{}\n", result.code).as_bytes(),
    )?;
    ledger.map_err(|e| error(e.code, e))?;
    Ok(result.code)
}

fn shell_words(raw: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut active = false;
    let mut chars = raw.chars().peekable();
    let mut ansi = false;
    while let Some(c) = chars.next() {
        if quote.is_none() && c.is_whitespace() {
            if active {
                result.push(std::mem::take(&mut word));
                active = false;
            }
            continue;
        }
        active = true;
        if quote.is_none() && c == '$' && chars.peek() == Some(&'\'') {
            chars.next();
            quote = Some('\'');
            ansi = true;
            continue;
        }
        if c == '\\' && (quote != Some('\'') || ansi) {
            let next = chars
                .next()
                .ok_or_else(|| error(2, "incomplete stored argv escape"))?;
            word.push(if ansi {
                match next {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                }
            } else {
                next
            });
            continue;
        }
        if c == '\'' || c == '"' {
            if quote == Some(c) {
                quote = None;
                ansi = false;
                continue;
            }
            if quote.is_none() {
                quote = Some(c);
                continue;
            }
        }
        word.push(c);
    }
    require(quote.is_none(), "unclosed stored argv quote")?;
    if active {
        result.push(word);
    }
    Ok(result)
}
