//! Capsules execute once with merged output, independently of companion ledger publication.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, epoch, error, helpers, json, json_output, require,
    spine, storage, success, timestamp,
};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub(crate) const HELP: &str = "Usage: oms run capsule run [--note TEXT] [--config PATH]... [--output PATH]... [--seed N]... [--data NAME]... [--metrics PATH] [--no-ledger] [--ledger PATH] -- COMMAND...\n       oms run capsule list [N] [--json]\n       oms run capsule show|reproduce|verify ID\n       oms run capsule whence FILE\nCapsules are local provenance, never reproduction evidence. Commands execute once.\n";

pub(crate) fn directory(ctx: &Context<'_>) -> std::path::PathBuf {
    std::env::var_os("OMS_RUNS_DIR")
        .filter(|p| !p.is_empty())
        .map(|p| helpers::absolute(&ctx.cwd, p))
        .unwrap_or_else(|| ctx.state_repo.join(".oms/runs"))
}

pub(crate) fn load(ctx: &Context<'_>, id: &str) -> Result<(Value, String)> {
    helpers::identifier(id)?;
    let bytes = storage::input(
        &directory(ctx).join(id).join("capsule.json"),
        storage::MAX_FILE,
    )?;
    let value = json::parse(&bytes)?;
    require(
        value.text("id") == id && value.get("schema").is_int(1),
        "capsule id or schema does not match",
    )?;
    Ok((value, json::sha(bytes)))
}

pub(crate) fn resolve(ctx: &Context<'_>, id: &str) -> Result<(String, Value, String)> {
    if let Ok((value, digest)) = load(ctx, id) {
        return Ok((id.into(), value, digest));
    }
    let (rows, _) = storage::rows(&ctx.state_repo, &spine::index(ctx))?;
    for row in rows
        .iter()
        .rev()
        .filter(|r| r.text("run_id") == id && r.text("tool") == "run-capsule")
    {
        let path = Path::new(row.text("path"));
        let target = helpers::absolute(&ctx.cwd, path);
        if let Ok(raw) = storage::input(&target, storage::MAX_FILE)
            && let Ok(value) = json::parse(&raw)
            && helpers::identifier(value.text("id")).is_ok()
            && value.get("schema").is_int(1)
        {
            return Ok((value.text("id").to_owned(), value, json::sha(raw)));
        }

        if let Some(name) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            && let Ok((value, digest)) = load(ctx, name)
        {
            return Ok((name.into(), value, digest));
        }
    }
    Err(error(2, "no capsule recorded for run"))
}

fn git(ctx: &Context<'_>) -> Result<(Value, Vec<u8>)> {
    let query = |args: &[&str], fallback: &str| {
        helpers::query(ctx, args)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim_end().to_owned())
            .unwrap_or_else(|| fallback.into())
    };
    let commit = query(&["git", "rev-parse", "HEAD"], "none");
    let short = query(&["git", "rev-parse", "--short", "HEAD"], "none");
    let branch = query(
        &["git", "symbolic-ref", "--quiet", "--short", "HEAD"],
        "detached",
    );
    let status = query(
        &["git", "status", "--porcelain", "--untracked-files=no"],
        "",
    );
    let dirty = status.lines().count();
    let diff = if dirty > 0 {
        helpers::query(ctx, &["git", "diff", "HEAD"])?
    } else {
        vec![]
    };
    Ok((
        object([
            ("commit_full", string(commit)),
            ("commit_short", string(short)),
            ("branch", string(branch)),
            ("dirty", number(dirty)),
            (
                "diff_sha256",
                string(if diff.is_empty() {
                    String::new()
                } else {
                    json::sha(&diff)[..16].into()
                }),
            ),
            (
                "diff_file",
                string(if diff.is_empty() {
                    ""
                } else {
                    "uncommitted.diff"
                }),
            ),
            (
                "status_short",
                string(status.lines().take(40).collect::<Vec<_>>().join("\n")),
            ),
        ]),
        diff,
    ))
}

pub(crate) fn verifies(ctx: &Context<'_>, capsule: &Value) -> Result<bool> {
    let (current, _) = git(ctx)?;
    Ok(["commit_full", "diff_sha256"]
        .iter()
        .all(|k| capsule.get("git").get(k) == current.get(k)))
}

struct Capture {
    file: std::fs::File,
}
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write_all(bytes)?;
        std::io::stderr().write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        std::io::stderr().flush()
    }
}

fn manifest_dir(ctx: &Context<'_>) -> std::path::PathBuf {
    std::env::var_os("OMS_MANIFEST_DIR")
        .filter(|s| !s.is_empty())
        .map(|s| helpers::absolute(&ctx.cwd, s))
        .unwrap_or_else(|| ctx.state_repo.join(".oms/manifests"))
}

fn data(ctx: &Context<'_>, names: &[String]) -> Result<Value> {
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        helpers::identifier(name)?;
        if !seen.insert(name) {
            continue;
        }
        let path = manifest_dir(ctx).join(format!("{name}.json"));
        let raw = storage::input(&path, storage::MAX_FILE)?;
        let mut manifest = json::parse(&raw)?;
        let stamp = manifest.get("ts").clone();
        let policy = manifest.get("policy").clone();
        manifest.remove("ts");
        manifest.remove("note");
        let mut splits = json::empty();
        for row in manifest
            .get("splits")
            .array()
            .unwrap_or(&[])
            .iter()
            .take(64)
        {
            if let Ok(label) = row.get("label").str() {
                splits.set(label, row.get("rows").clone())?;
            }
        }
        let mut row = object([
            ("name", string(name)),
            ("manifest_sha256", string(json::hash(&manifest)?)),
            ("manifest_ts", stamp),
            ("splits", splits),
        ]);
        if policy.truth() {
            row.set("policy", policy)?;
        }
        rows.push(row);
    }
    Ok(Value::Array(rows))
}

fn flatten(value: &Value, prefix: &str, result: &mut Value) -> Result<()> {
    if let Value::Object(fields) = value
        && !fields.is_empty()
    {
        for (key, value) in fields {
            let key = std::str::from_utf8(key).map_err(|_| error(2, "config key is not UTF-8"))?;
            flatten(
                value,
                &if prefix.is_empty() {
                    key.into()
                } else {
                    format!("{prefix}.{key}")
                },
                result,
            )?;
        }
    } else {
        require(
            result.object()?.len() < 200 && prefix.chars().count() <= 200,
            "config parameter bound exceeded",
        )?;
        let value = if matches!(value, Value::Array(_) | Value::Object(_)) {
            string(String::from_utf8(json::canonical(value)?).map_err(|e| error(2, e))?)
        } else {
            value.clone()
        };
        require(
            helpers::display(&value).chars().count() <= 200,
            "config value bound exceeded",
        )?;
        result.set(prefix, value)?;
    }
    Ok(())
}
fn configs(ctx: &Context<'_>, paths: &[String]) -> Result<Value> {
    let mut rows = Vec::new();
    for path in paths {
        let target = helpers::absolute(&ctx.cwd, path);
        if target
            .symlink_metadata()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            eprintln!("capsule: config not found, skipped: {path}");
            continue;
        }
        let bytes = storage::input(&target, storage::MAX_FILE)?;
        let mut row = object([
            ("path", string(path)),
            ("sha256", string(&json::sha(&bytes)[..32])),
        ]);
        let mut flat = json::empty();
        let status = if bytes.len() > 1024 * 1024 {
            "too-large"
        } else if let Ok(value) = json::parse(&bytes) {
            if flatten(&value, "", &mut flat).is_err() || json::canonical(&flat)?.len() > 16384 {
                "bounds"
            } else if helpers::scrub(&flat).is_err() {
                "withheld-sensitive"
            } else {
                row.set("params", flat)?;
                "recorded"
            }
        } else {
            "unparsed"
        };
        row.set("params_status", string(status))?;
        rows.push(row);
    }
    Ok(Value::Array(rows))
}

fn outputs(ctx: &Context<'_>, paths: &[String]) -> Result<Value> {
    let mut rows = Vec::new();
    for path in paths {
        let target = helpers::absolute(&ctx.cwd, path);
        let meta = std::fs::symlink_metadata(&target);
        let mut row = object([
            ("path", string(path)),
            ("exists", Value::Bool(meta.is_ok())),
            ("hashed", Value::Bool(false)),
        ]);
        if let Ok(meta) = meta {
            row.set("size", number(meta.len()))?;
            row.set(
                "mtime",
                number(
                    meta.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                ),
            )?;
            if meta.is_file() && meta.len() <= 512 * 1024 * 1024 {
                match storage::hash_input(&target, 512 * 1024 * 1024) {
                    Ok(digest) => {
                        row.set("sha256", string(digest))?;
                        row.set("hashed", Value::Bool(true))?;
                    }
                    Err(_) => {
                        eprintln!("capsule: output could not be hashed: {path}");
                    }
                }
            }
        }
        rows.push(row);
    }
    Ok(Value::Array(rows))
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
    let end = split.unwrap_or(args.len());
    let opts = Options::parse(
        &args[1..end],
        &[
            "note", "config", "output", "seed", "data", "metrics", "ledger",
        ],
        &["no-ledger", "json"],
    )?;
    if opts.flag("help") {
        return Ok(success(HELP));
    }
    if action == "run" {
        require(opts.pos.is_empty(), "run takes options before --")?;
        let command = split
            .map(|i| &args[i + 1..])
            .filter(|v| !v.is_empty())
            .ok_or_else(|| error(2, "run requires a command after --"))?;
        for key in ["config", "output", "seed", "data"] {
            require(
                opts.all(key).len() <= 128,
                "capsule repeated option limit exceeded",
            )?;
        }
        let data = data(ctx, opts.all("data"))?;
        let configs = configs(ctx, opts.all("config"))?;
        let (git, diff) = git(ctx)?;
        let seeds = json::list(opts.all("seed").iter().map(String::as_str));
        let id = format!(
            "{}-{}",
            timestamp(epoch())?.replace(['-', ':'], ""),
            &crate::random_id("")?[..12]
        );
        let ledger = if opts.get("ledger").is_empty() {
            ctx.repo.join("docs/EXPERIMENTS.jsonl")
        } else {
            ctx.cwd.join(opts.get("ledger"))
        };
        let contract = oms_command_runs::metrics::row_contract(&ledger, &ctx.repo)
            .map_err(|e| error(e.code, e))?
            .unwrap_or_else(|| oms_context::Json::Object(vec![]));
        let capture = oms_command_runs::metrics::capture_for_ledger(
            &helpers::context(&git)?,
            &helpers::context(&configs)?,
            &helpers::context(&data)?,
            &helpers::context(&json::list(opts.all("config").iter().map(String::as_str)))?,
            &contract,
            &helpers::context(&seeds)?,
            &id,
        )
        .map_err(|e| error(e.code, e))?;
        let capture_value = helpers::from_context(&capture);
        let bundle = directory(ctx).join(&id);
        let _guard = storage::lock(&bundle.join("capsule.json"))?;
        if !diff.is_empty() {
            storage::replace_locked(&ctx.state_repo, &bundle.join("uncommitted.diff"), &diff)?;
        }
        let started = epoch();
        storage::replace_locked(&ctx.state_repo, &bundle.join("output.log"), b"")?;
        let env = environment(ctx, &bundle)?;
        if opts.all("data").is_empty()
            && let Ok(entries) = std::fs::read_dir(manifest_dir(ctx))
        {
            let mut names = entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|s| s == "json"))
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>();
            names.sort();
            if !names.is_empty() {
                eprintln!(
                    "capsule: data manifests present ({}); record provenance with --data NAME",
                    names.join(", ")
                );
            }
        }
        for output in opts.all("output") {
            let path = helpers::absolute(&ctx.cwd, output);
            if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                return Err(error(2, "output is a symlink; command not started"));
            }
        }
        if !opts.get("metrics").is_empty() {
            let path = helpers::absolute(&ctx.cwd, opts.get("metrics"));
            if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                return Err(error(2, "metrics is a symlink; command not started"));
            }
        }
        let mut console = Capture {
            file: storage::log_file(&bundle.join("output.log"))?,
        };
        let status = execute_merged(command, &ctx.cwd, ctx.interrupted, None, &mut console)?;
        let duration = epoch() - started;
        console.file.sync_all().map_err(|e| error(1, e))?;
        let metrics = if opts.get("metrics").is_empty() {
            Value::Null
        } else {
            let max = std::env::var("OMS_METRICS_MAX_BYTES")
                .ok()
                .map(|s| {
                    s.parse::<usize>()
                        .map_err(|_| error(2, "invalid OMS_METRICS_MAX_BYTES"))
                })
                .transpose()?
                .unwrap_or(65536)
                .min(storage::MAX_FILE);
            storage::input(&helpers::absolute(&ctx.cwd, opts.get("metrics")), max)
                .ok()
                .and_then(|b| json::parse(&b).ok())
                .filter(|v| v.object().is_ok())
                .unwrap_or(Value::Null)
        };
        let mut capsule = object([
            ("schema", number(1)),
            ("id", string(&id)),
            ("ts", string(timestamp(started)?)),
            ("agent", string(helpers::agent())),
            ("cwd", string(ctx.cwd.to_string_lossy())),
            ("note", string(opts.get("note"))),
            ("git", git.clone()),
            ("env", env),
            ("configs", configs),
            ("outputs", outputs(ctx, opts.all("output"))?),
            ("seeds", seeds),
            (
                "slurm_job_id",
                string(std::env::var("SLURM_JOB_ID").unwrap_or_default()),
            ),
            (
                "command",
                json::list(crate::strings(command)?.iter().map(String::as_str)),
            ),
            ("log_file", string("output.log")),
            (
                "result",
                object([
                    ("exit", number(status)),
                    ("duration_s", number(duration)),
                    ("metrics", metrics),
                ]),
            ),
            ("input_identity", capture_value.get("inputs").clone()),
            (
                "metric_contract",
                capture_value.get("metric_contract").clone(),
            ),
        ]);
        if data.truth() {
            capsule.set("data", data)?;
        }
        helpers::trace(&mut capsule, None)?;
        storage::replace_locked(
            &ctx.state_repo,
            &bundle.join("capsule.json"),
            &json::encoded(&capsule)?,
        )?;
        let row = object([
            ("id", string(&id)),
            ("ts", capsule.get("ts").clone()),
            ("exit", number(status)),
            ("duration_s", number(duration)),
            ("cwd", capsule.get("cwd").clone()),
            ("git_sha", git.get("commit_short").clone()),
            ("dirty", git.get("dirty").clone()),
            ("command", capsule.get("command").clone()),
        ]);
        storage::append(&ctx.state_repo, &directory(ctx).join("index.jsonl"), &row)?;
        let mut diagnostics = Vec::new();
        if !opts.flag("no-ledger") {
            let mut native = oms_command_runs::Context::new(&ctx.cwd, ctx.interrupted)
                .map_err(|e| error(e.code, e))?;
            native.env.insert(
                "OMS_RUN_LEDGER_STATUS_OVERRIDE".into(),
                status.to_string().into(),
            );
            native.env.insert(
                "OMS_RUN_LEDGER_CAPTURED".into(),
                String::from_utf8(json::canonical(&capture_value)?)
                    .map_err(|e| error(2, e))?
                    .into(),
            );
            native
                .env
                .insert("OMS_WORK_JOURNAL_SUPPRESS".into(), "1".into());
            let mut argv: Vec<OsString> = vec![
                "--note".into(),
                opts.get("note").into(),
                "--no-gate".into(),
                "--file".into(),
                ledger.into_os_string(),
            ];
            if !opts.get("metrics").is_empty() {
                argv.extend(["--metrics".into(), opts.get("metrics").into()]);
            }
            argv.push("--".into());
            argv.extend_from_slice(command);
            if let Err(e) = oms_command_runs::execute(
                &argv,
                &native,
                services,
                &mut std::io::sink(),
                &mut std::io::sink(),
            ) {
                diagnostics.extend_from_slice(
                    format!("capsule: companion ledger row failed: {e}; capsule still saved\n")
                        .as_bytes(),
                );
            }
        }
        if let Ok(run_id) = spine::current(ctx) {
            spine::link(
                ctx,
                &run_id,
                "run-capsule",
                "capture",
                &bundle.join("capsule.json").to_string_lossy(),
                &format!("exit {status}"),
                &helpers::agent(),
            )?;
        }
        if services.available(oms_command_runs::Service::Journal)
            && let Err(e) = services.observe(oms_command_runs::Observation {
                repo: &ctx.repo,
                source_type: "run-capsule",
                record_path: &bundle.join("capsule.json"),
                row: &helpers::context(&capsule)?,
                event_type: None,
            })
        {
            diagnostics.extend_from_slice(
                format!("capsule: journal observation failed: {e}\n").as_bytes(),
            );
        }
        diagnostics.extend_from_slice(
            format!(
                "capsule: {} (exit {status}, {duration}s)\n",
                bundle.join("capsule.json").display()
            )
            .as_bytes(),
        );
        return Ok((status, format!("{id}\n").into_bytes(), diagnostics));
    }
    require(split.is_none(), "unexpected command separator")?;
    match action {
        "list" => {
            require(opts.pos.len() <= 1, "list takes at most N")?;
            let n = opts
                .pos
                .first()
                .map(|s| s.parse::<usize>())
                .transpose()
                .map_err(|_| error(2, "list requires a non-negative integer"))?
                .unwrap_or(10);
            let (mut rows, _) =
                storage::rows(&ctx.state_repo, &directory(ctx).join("index.jsonl"))?;
            let rows = rows.split_off(rows.len().saturating_sub(n));
            if opts.flag("json") {
                return json_output(
                    0,
                    &object([("schema", number(1)), ("capsules", Value::Array(rows))]),
                );
            }
            let mut out = String::new();
            for row in rows {
                out.push_str(&format!(
                    "{} exit={} {}\n",
                    row.text("id"),
                    row.get("exit").int().unwrap_or(-1),
                    row.get("command").strings().unwrap_or_default().join(" ")
                ));
            }
            Ok(success(out))
        }
        "show" | "verify" | "reproduce" => {
            require(opts.pos.len() == 1, "action requires one capsule id")?;
            let (capsule, _) = load(ctx, &opts.pos[0])?;
            match action {
                "show" => json_output(0, &capsule),
                "verify" => {
                    let (current, _) = git(ctx)?;
                    let old = capsule.get("git");
                    let mut out = String::new();
                    let mut drift = false;
                    for (field, label) in [
                        ("commit_full", "commit"),
                        ("diff_sha256", "uncommitted-diff"),
                    ] {
                        let ok = old.get(field) == current.get(field);
                        drift |= !ok;
                        out.push_str(&format!(
                            "{} {label}: capsule={} current={}\n",
                            if ok { "ok  " } else { "DRIFT" },
                            helpers::display(old.get(field)),
                            helpers::display(current.get(field))
                        ));
                    }
                    for (key, code) in [
                        ("python", "import platform;print(platform.python_version())"),
                        ("torch", "import torch;print(torch.__version__)"),
                        ("cuda", "import torch;print(torch.version.cuda)"),
                    ] {
                        let python = capsule.get("env").text("interpreter");
                        let python = if python.is_empty() { "python3" } else { python };
                        let now = helpers::query(ctx, &[python, "-c", code])
                            .ok()
                            .map(|b| string(String::from_utf8_lossy(&b).trim()))
                            .unwrap_or(Value::Null);
                        let prior = capsule.get("env").get(key);
                        out.push_str(&format!(
                            "{} env-{key}: capsule={} current={}\n",
                            if prior == &now { "ok   " } else { "WARN " },
                            helpers::display(prior),
                            helpers::display(&now)
                        ));
                    }
                    for old in capsule.get("data").array().unwrap_or(&[]) {
                        let name = old.text("name");
                        let now = data(ctx, &[name.to_owned()]).ok();
                        let same = now
                            .as_ref()
                            .and_then(|v| v.array().ok())
                            .and_then(|a| a.first())
                            .is_some_and(|r| {
                                r.get("manifest_sha256") == old.get("manifest_sha256")
                            });
                        out.push_str(&format!(
                            "{} data-{name}: capsule={}\n",
                            if same { "ok   " } else { "WARN " },
                            old.text("manifest_sha256")
                        ));
                    }
                    Ok((i32::from(drift), out.into_bytes(), vec![]))
                }
                _ => Ok(success(reproduce(ctx, &capsule)?)),
            }
        }
        "whence" => {
            require(opts.pos.len() == 1, "whence requires one file")?;
            let target = helpers::absolute(&ctx.cwd, &opts.pos[0]);
            let digest = storage::hash_input(&target, u64::MAX)?;
            let mut out = String::new();
            for path in helpers::files(ctx, &directory(ctx), "capsule.json", 50000)? {
                let Ok(bytes) = storage::input(&path, storage::MAX_FILE) else {
                    continue;
                };
                let Ok(cap) = json::parse(&bytes) else {
                    continue;
                };
                for row in cap.get("outputs").array().unwrap_or(&[]) {
                    let hashed = row.text("sha256") == digest;
                    let by_path = !row.get("hashed").truth()
                        && helpers::absolute(Path::new(cap.text("cwd")), row.text("path"))
                            == target;
                    if hashed || by_path {
                        out.push_str(&format!(
                            "{}  {}  ({} match: {})\n",
                            cap.text("id"),
                            cap.text("ts"),
                            if hashed { "sha" } else { "path" },
                            row.text("path")
                        ));
                    }
                }
            }
            if out.is_empty() {
                Ok((
                    1,
                    vec![],
                    b"no run produced this file (no matching output sha)\n".to_vec(),
                ))
            } else {
                Ok(success(out))
            }
        }

        _ => Err(error(2, HELP)),
    }
}

fn quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

/// A no-ledger capture needs the same child ownership and cancellation protocol
/// as native runs, without entering the ledger-producing execution adapter.
pub fn execute_merged(
    command: &[OsString],
    cwd: &Path,
    interrupted: &AtomicBool,
    timeout: Option<Duration>,
    output: &mut dyn Write,
) -> Result<i32> {
    let context = oms_command_runs::Context::new(cwd, interrupted).map_err(|e| error(e.code, e))?;
    let result = oms_command_runs::process::merged(&context, command, timeout, output)
        .map_err(|e| error(e.code, e))?;
    Ok(result.code)
}

fn environment(ctx: &Context<'_>, bundle: &Path) -> Result<Value> {
    let python = std::env::var("OMS_PROJECT_PYTHON")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            for base in [".venv", "venv"] {
                let path = ctx.repo.join(base).join("bin/python");
                if path.is_file() {
                    return path.to_string_lossy().into_owned();
                }
            }
            "python3".into()
        });
    let probe = |argv: &[&str]| {
        helpers::query(ctx, argv)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let mut env = object([
        ("interpreter", string(&python)),
        (
            "python",
            probe(&[
                &python,
                "-c",
                "import platform;print(platform.python_version())",
            ])
            .map(string)
            .unwrap_or(Value::Null),
        ),
        (
            "torch",
            probe(&[&python, "-c", "import torch;print(torch.__version__)"])
                .map(string)
                .unwrap_or(Value::Null),
        ),
        (
            "cuda",
            probe(&[&python, "-c", "import torch;print(torch.version.cuda)"])
                .map(string)
                .unwrap_or(Value::Null),
        ),
        (
            "host",
            probe(&["hostname", "-s"])
                .map(|s| string(s.chars().take(64).collect::<String>()))
                .unwrap_or(Value::Null),
        ),
        (
            "cpu_count",
            number(
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(1),
            ),
        ),
        (
            "modules",
            std::env::var("LOADEDMODULES")
                .ok()
                .map(|s| json::list(s.split(':').filter(|s| !s.is_empty()).take(128)))
                .filter(Value::truth)
                .unwrap_or(Value::Null),
        ),
        ("conda", Value::Null),
        ("env_vars", json::empty()),
    ]);
    let mut variables = json::empty();
    for name in [
        "CUDA_VISIBLE_DEVICES",
        "OMP_NUM_THREADS",
        "SLURM_JOB_ID",
        "SLURM_ARRAY_TASK_ID",
        "SLURM_JOB_NODELIST",
        "SLURM_JOB_PARTITION",
    ] {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            variables.set(
                name,
                string(
                    value
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(256)
                        .collect::<String>(),
                ),
            )?;
        }
    }
    env.set(
        "env_vars",
        if variables.object()?.is_empty() {
            Value::Null
        } else {
            variables
        },
    )?;
    if let Ok(name) = std::env::var("CONDA_DEFAULT_ENV")
        && !name.is_empty()
    {
        let urls = regex::Regex::new(r"://[^/\s@]*@|/t/[^/\s]+/").expect("static expression");
        for tool in ["conda", "mamba"] {
            if let Some(body) = probe(&[tool, "env", "export", "--no-builds"]) {
                let mut clean = String::new();
                let mut skipping = false;
                for line in body.lines() {
                    if line.starts_with("prefix:") {
                        continue;
                    }
                    if line.starts_with("variables:") {
                        skipping = true;
                        continue;
                    }
                    if skipping && (line.starts_with([' ', '\t', '-']) || line.trim().is_empty()) {
                        continue;
                    }
                    skipping = false;
                    clean.push_str(line);
                    clean.push('\n');
                }
                let mut conda = object([
                    ("env", string(&name)),
                    ("tool", string(tool)),
                    (
                        "export_sha256",
                        if clean.trim().is_empty() {
                            Value::Null
                        } else {
                            string(json::sha(clean.as_bytes()))
                        },
                    ),
                ]);
                if clean.len() <= 1024 * 1024
                    && !urls.is_match(&clean)
                    && helpers::scrub(&string(&clean)).is_ok()
                {
                    storage::replace_locked(
                        &ctx.state_repo,
                        &bundle.join("env.conda.yml"),
                        clean.as_bytes(),
                    )?;
                } else {
                    conda.set("withheld", Value::Bool(true))?;
                }
                env.set("conda", conda)?;
                break;
            }
        }
    }
    if let Ok(bytes) = storage::input(&ctx.repo.join("uv.lock"), storage::MAX_FILE) {
        storage::replace_locked(&ctx.state_repo, &bundle.join("env.uv.lock"), &bytes)?;
    } else if let Some(freeze) = probe(&[&python, "-m", "pip", "freeze"])
        && helpers::scrub(&string(&freeze)).is_ok()
    {
        env.set("freeze_sha256", string(json::sha(freeze.as_bytes())))?;
        storage::replace_locked(
            &ctx.state_repo,
            &bundle.join("env.freeze.txt"),
            freeze.as_bytes(),
        )?;
    }
    Ok(env)
}

fn reproduce(ctx: &Context<'_>, cap: &Value) -> Result<String> {
    let id = cap.text("id");
    let bundle = directory(ctx).join(id);
    let git = cap.get("git");
    let env = cap.get("env");
    let mut out = format!(
        "# Reproduce capsule {id} (captured {} by {})\ncd {}\n",
        cap.text("ts"),
        cap.text("agent"),
        quote(cap.text("cwd"))
    );
    if !["none", "no-commit", ""].contains(&git.text("commit_full")) {
        out.push_str(&format!(
            "git checkout {}\n",
            quote(git.text("commit_full"))
        ));
    }
    if !git.text("diff_file").is_empty() {
        out.push_str(&format!(
            "git apply {}   # restore uncommitted changes at capture time\n",
            quote(&bundle.join(git.text("diff_file")).to_string_lossy())
        ));
    }
    if bundle.join("env.uv.lock").exists() {
        out.push_str(&format!(
            "uv sync --frozen   # from {}/env.uv.lock\n",
            bundle.display()
        ));
    } else if bundle.join("env.freeze.txt").exists() {
        out.push_str(&format!(
            "uv pip install -r {}   # or: pip install -r ...\n",
            quote(&bundle.join("env.freeze.txt").to_string_lossy())
        ));
    }
    out.push_str(&format!(
        "# env at capture: python={} torch={} cuda={}\n",
        helpers::display(env.get("python")),
        helpers::display(env.get("torch")),
        helpers::display(env.get("cuda"))
    ));
    if env.get("conda").object().is_ok() {
        out.push_str(&format!(
            "# conda env at capture: {}{}\n",
            env.get("conda").text("env"),
            if bundle.join("env.conda.yml").exists() {
                format!(
                    "; recreate with: conda env create -f {}",
                    quote(&bundle.join("env.conda.yml").to_string_lossy())
                )
            } else {
                " (export not saved)".into()
            }
        ));
    }
    if let Ok(modules) = env.get("modules").strings()
        && !modules.is_empty()
    {
        out.push_str(&format!("# modules at capture: {}\n", modules.join(" ")));
    }
    if let Ok(seeds) = cap.get("seeds").strings()
        && !seeds.is_empty()
    {
        out.push_str(&format!("# seeds: {}\n", seeds.join(", ")));
    }
    for row in cap.get("data").array().unwrap_or(&[]) {
        out.push_str(&format!(
            "# data at capture: {} manifest {}; confirm with: oms data-manifest check --name {}\n",
            row.text("name"),
            row.text("manifest_sha256")
                .chars()
                .take(12)
                .collect::<String>(),
            row.text("name")
        ));
    }
    out.push_str("# command:\n");
    out.push_str(
        &cap.get("command")
            .strings()?
            .into_iter()
            .map(quote)
            .collect::<Vec<_>>()
            .join(" "),
    );
    out.push('\n');
    Ok(out)
}
