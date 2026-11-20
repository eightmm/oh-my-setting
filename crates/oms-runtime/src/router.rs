//! Common parser/dispatch for CLI, panel and MCP. No caller supplies runtime outputs.
use crate::parser::{Mode, Request};
use crate::storage::Runtime;
use crate::value::*;
use crate::{Result, Services, error, require};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::atomic::AtomicBool;
#[derive(Debug, Default)]
pub struct Response {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl Response {
    pub fn failure(error: crate::Error) -> Self {
        Self {
            code: error.code,
            stdout: vec![],
            stderr: if error.message.starts_with("usage: oms runtime") {
                error.message.into_bytes()
            } else {
                format!("runtime-core: {error}\n").into_bytes()
            },
        }
    }
    pub fn text(value: impl AsRef<str>) -> Self {
        let mut stdout = value.as_ref().as_bytes().to_vec();
        if !stdout.ends_with(b"\n") {
            stdout.push(b'\n')
        }
        Self {
            stdout,
            ..Default::default()
        }
    }
    pub fn json(value: &Value, pretty: bool, code: i32) -> Result<Self> {
        let mut stdout = if pretty {
            sorted_pretty(value)?
        } else {
            canonical(value)?
        };
        stdout.push(b'\n');
        Ok(Self {
            code,
            stdout,
            stderr: vec![],
        })
    }
    pub fn native((code, stdout, stderr): (i32, Vec<u8>, Vec<u8>)) -> Self {
        Self {
            code,
            stdout,
            stderr,
        }
    }
}
/// Resolve roots with the caller's native producer, then execute the shared core.
/// A failed action is final: no exit value authorizes a legacy retry.
pub fn run(
    argv: &[OsString],
    cwd: &Path,
    install_root: &Path,
    services: &dyn Services,
    interrupted: &AtomicBool,
) -> Response {
    if argv.first().and_then(|s| s.to_str()) == Some("ssh-adapter") {
        return crate::ssh_adapter::run(&argv[1..], cwd, interrupted);
    }
    if argv.first().and_then(|s| s.to_str()) == Some("exec-snapshot") {
        return crate::exec_snapshot::run(&argv[1..], cwd, install_root, services, interrupted);
    }
    let result = (|| {
        let request = crate::parse(argv)?;
        if request.has("help") || request.mode == Mode::Help {
            return Ok(Response::text(crate::parser::help(&request)));
        }
        if request.mode == Mode::Version {
            return Ok(Response::text(crate::CORE_VERSION));
        }
        authorize(&request)?;
        let roots = services.resolve_roots(cwd, &request.repo)?;
        let mut runtime = Runtime::open(
            &roots.execution_repo,
            &roots.state_repo,
            install_root,
            services,
            interrupted,
        )?;
        runtime.cwd = cwd.to_path_buf();
        dispatch(&runtime, &request)
    })();
    result.unwrap_or_else(Response::failure)
}
fn authorize(r: &Request) -> Result<()> {
    require(
        std::env::var_os("OMS_HARNESS_CHILD").as_deref() != Some(OsStr::new("1"))
            || r.child_read_only(),
        "a harness child may only use read-only runtime actions",
    )
}
fn strings(args: &[OsString]) -> Result<Vec<String>> {
    args.iter()
        .map(|v| {
            v.to_str()
                .map(str::to_owned)
                .ok_or_else(|| error("argument requires Unicode text"))
        })
        .collect()
}
fn path_option(rt: &Runtime<'_>, r: &Request, key: &str) -> Result<Option<std::path::PathBuf>> {
    r.os(key)
        .filter(|s| !s.is_empty())
        .map(|s| rt.user_path(s))
        .transpose()
}
fn report(rt: &Runtime<'_>, r: &Request) -> Result<Response> {
    let limit = r.integer("limit", 20)?;
    require((1..=100).contains(&limit), "limit must be from 1 to 100")?;
    let format = r.text("format", "json")?;
    require(
        !(format == "text" && r.pretty),
        "--pretty applies to JSON, not --format text",
    )?;
    let envelope = crate::projection::envelope(rt)?;
    let snapshot = to_native(&envelope);
    let task = r
        .os("task")
        .map(|s| {
            s.to_str()
                .ok_or_else(|| error("task requires Unicode text"))
        })
        .transpose()?;
    let report = if r.mode == Mode::Evidence {
        oms_work_inspect::evidence_matrix_value(&snapshot, limit as usize, task)
    } else if r.mode == Mode::Next {
        oms_work_inspect::action_inbox_value(&snapshot, limit as usize, task)
    } else {
        let before = r
            .os("before")
            .map(|p| {
                let path = std::path::PathBuf::from(p);
                let root = oms_safe_fs::Root::open(rt.source_root(&path))
                    .map_err(|_| error("cannot safely read the selected prior envelope"))?;
                let raw = root
                    .read_manifest(&path, 4 * 1024 * 1024)
                    .map_err(|_| error("cannot safely read the selected prior envelope"))?;
                let mut value = oms_context::parse_json(&raw).map_err(|message| {
                    if message == "cannot read a valid repo-local context manifest" {
                        error("cannot read a valid prior work snapshot")
                    } else {
                        error(message.replace("context manifest", "work snapshot"))
                    }
                })?;
                match &mut value {
                    oms_context::Json::Object(entries)
                        if entries.iter().any(|(key, _)| key == b"envelope") =>
                    {
                        require(
                            entries.len() == 2 && entries.iter().any(|(key, _)| key == b"written"),
                            "invalid envelope-write acknowledgement",
                        )?;
                        let index = entries
                            .iter()
                            .position(|(key, _)| key == b"envelope")
                            .expect("validated acknowledgement envelope");
                        Ok(entries.swap_remove(index).1)
                    }
                    _ => Ok(value),
                }
            })
            .transpose()?;
        oms_work_inspect::return_brief_value(&snapshot, before.as_ref(), limit as usize)
    }
    .map_err(error)?;
    if format == "text" {
        Ok(Response::text(
            oms_work_inspect::render(&report).map_err(error)?,
        ))
    } else {
        Response::json(&from_serde(report.as_value().map_err(error)?)?, r.pretty, 0)
    }
}
pub fn dispatch(rt: &Runtime<'_>, r: &Request) -> Result<Response> {
    authorize(r)?;
    rt.check()?;
    if r.has("help") {
        return Ok(Response::text(crate::parser::help(r)));
    }
    if r.mode == Mode::Version {
        return Ok(Response::text(crate::CORE_VERSION));
    }
    if r.mode == Mode::Brief
        || r.mode == Mode::Next && r.has("explain")
        || r.mode == Mode::Evidence && r.action == "matrix"
    {
        return report(rt, r);
    }
    let mut code = 0;
    let mut stderr = vec![];
    let mut stdout_prefix = vec![];
    let value = match r.mode {
        Mode::Envelope => {
            let value = crate::projection::envelope(rt)?;
            if r.action == "write" {
                let output = r
                    .os("output")
                    .unwrap_or(OsStr::new(".oms/runtime/effective-contract.json"));
                let output = rt.user_path(output)?;
                if r.has("output") {
                    rt.write_json_output(&output, &value)?;
                } else {
                    rt.write_json(&output, &value)?;
                }
                object([("written", rt.output_label(&output)), ("envelope", value)])
            } else {
                value
            }
        }
        Mode::Resources => crate::resources::action(rt, r)?,
        Mode::Evidence => match r.action.as_str() {
            "show" => {
                let base = crate::projection::base(rt)?;
                crate::evidence::coverage(rt, &base)?
            }
            "unbound" => {
                let base = crate::projection::base(rt)?;
                let coverage = crate::evidence::coverage(rt, &base)?;
                pick(&coverage, &["schema", "unbound_evidence"])
            }
            "bind" => {
                let dependencies = strings(&r.all("depends"))?;
                crate::evidence::bind(
                    rt,
                    crate::evidence::Binding {
                        criterion: r.text("criterion", "")?,
                        reference: r.text("ref", "")?,
                        status: r.text("status", "")?,
                        evidence_type: r.text("type", "explicit-binding")?,
                        note: r.text("note", "")?,
                        dependencies: &dependencies,
                    },
                )?
            }
            "revoke" => crate::evidence::revoke(rt, r.text("binding", "")?, r.text("reason", "")?)?,
            _ => return Err(error("unknown evidence action")),
        },
        Mode::Next => {
            require(
                !r.has("task")
                    && r.integer("limit", 20)? == 20
                    && r.text("format", "json")? == "json",
                "next --task/--limit/--format require --explain",
            )?;
            let value = crate::projection::envelope(rt)?;
            object([
                ("schema", number(1)),
                ("state_digest", value.get("state_digest").clone()),
                ("actions", value.get("next_actions").clone()),
            ])
        }
        Mode::Failure => {
            let mut args = vec![
                OsString::from(&r.action),
                "--repo".into(),
                rt.repo.as_os_str().to_owned(),
            ];
            if r.pretty {
                args.push("--pretty".into())
            }
            args.extend(r.positionals.iter().cloned());
            for k in ["exit", "code"] {
                if let Some(v) = r.os(k) {
                    args.extend([format!("--{k}").into(), v.to_owned()])
                }
            }
            return Ok(Response::native(oms_command_failure::run(
                &args,
                &rt.repo,
                rt.interrupted,
            )));
        }
        Mode::Context => {
            let explicit = r
                .all("file")
                .iter()
                .map(|s| {
                    use std::os::unix::ffi::{OsStrExt, OsStringExt};
                    let raw = s.as_bytes();
                    if let Some(i) = raw
                        .iter()
                        .position(|b| *b == b':')
                        .filter(|i| !(*i == 1 && raw.len() > 2))
                    {
                        Ok((
                            OsString::from_vec(raw[..i].to_vec()),
                            String::from_utf8(raw[i + 1..].to_vec()).map_err(error)?,
                        ))
                    } else {
                        Ok((s.clone(), "explicit context".into()))
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            let max = r.integer("max-bytes", 65536)?;
            require(
                max >= 0,
                "context budget must be between 4096 and 524288 bytes",
            )?;
            let value = crate::context::plan(
                rt,
                &crate::context::ContextRequest {
                    targets: r.all("target"),
                    explicit,
                    required: r.all("require"),
                    max_bytes: usize::try_from(max).map_err(error)?,
                    bundle: path_option(rt, r, "bundle")?,
                    manifest: path_option(rt, r, "manifest")?,
                    allow_external: r.has("allow-external"),
                    phase: r.text("phase", "implementation")?.into(),
                },
            )?;
            if !value.get("sufficient").truth() {
                code = 3
            }
            value
        }
        Mode::ContextInspect => {
            let format = r.text("format", "json")?;
            require(
                !(format == "text" && r.pretty),
                "--pretty applies only to JSON output",
            )?;
            require(
                !(format == "text" && (r.has("injections") || r.has("session"))),
                "--format text renders a MANIFEST inspection or comparison",
            )?;
            let value = crate::context::inspect(rt, r)?;
            if format == "text" {
                let json = serde_json::from_slice(&canonical(&value)?).map_err(error)?;
                return Ok(Response::text(
                    oms_context_report::render_report(&json).map_err(error)?,
                ));
            }
            value
        }
        Mode::Profile => {
            let names = strings(&r.positionals)?;
            let value = match r.action.as_str() {
                "list" => object([
                    ("schema", number(1)),
                    ("profiles", crate::profiles::catalog(rt)?),
                ]),
                "show" => {
                    let name = names
                        .first()
                        .ok_or_else(|| error("profile name required"))?;
                    let catalog = crate::profiles::catalog(rt)?;
                    let profile = catalog
                        .opt(name)
                        .ok_or_else(|| error("unknown capability profile"))?;
                    object([
                        ("schema", number(1)),
                        ("name", string(name)),
                        ("profile", profile.clone()),
                    ])
                }
                "current" => crate::profiles::current(rt)?,
                "check" => crate::profiles::check(rt, &names)?,
                "apply" => crate::profiles::apply(rt, &names, r.has("allow-missing"))?,
                "install-plan" => {
                    crate::profiles::install_plan(rt, &names, r.text("primary-provider", "codex")?)?
                }
                "install" => {
                    let result = rt.services.install_profiles(
                        &rt.repo,
                        &crate::services::InstallerRequest {
                            profiles: names.clone(),
                            primary_provider: r
                                .os("primary-provider")
                                .map(|s| {
                                    s.to_str()
                                        .map(str::to_owned)
                                        .ok_or_else(|| error("invalid primary provider"))
                                })
                                .transpose()?,
                            upgrade: r.has("upgrade"),
                            allow_missing: r.has("allow-missing"),
                            receipt: path_option(rt, r, "receipt")?,
                            dry_run: r.has("dry-run"),
                        },
                    )?;
                    code = result.exit;
                    stderr.extend(
                        String::from_utf8_lossy(&result.stderr)
                            .trim_end()
                            .as_bytes(),
                    );
                    if !result.stderr.is_empty() {
                        stderr.push(b'\n')
                    };
                    let installer = if result.stdout.iter().all(u8::is_ascii_whitespace) {
                        Value::Null
                    } else {
                        parse(&result.stdout).unwrap_or_else(|_| {
                            let text = String::from_utf8_lossy(&result.stdout);
                            object([(
                                "output",
                                string(
                                    text.chars()
                                        .skip(text.chars().count().saturating_sub(4000))
                                        .collect::<String>(),
                                ),
                            )])
                        })
                    };
                    let selected = installer.text("primary_provider");
                    let plan = if selected.is_empty() {
                        Value::Null
                    } else {
                        crate::profiles::install_plan(rt, &names, selected)?
                    };
                    let configured = if code == 0 && !r.has("dry-run") {
                        crate::profiles::apply(rt, &names, r.has("allow-missing"))?
                    } else {
                        Value::Null
                    };
                    return {
                        let value = object([
                            ("schema", number(1)),
                            ("exit", number(code)),
                            ("installed", b(code == 0)),
                            ("plan", plan),
                            ("installer", installer),
                            ("configured", configured),
                        ]);
                        let mut response = Response::json(&value, r.pretty, code)?;
                        response.stderr = stderr;
                        Ok(response)
                    };
                }
                _ => return Err(error("unknown profile action")),
            };
            let ready = value.opt("ready").or(value.get("check").opt("ready"));
            if ready.is_some_and(|v| !v.truth()) {
                code = 3
            }
            value
        }
        Mode::Release => {
            let value = match r.action.as_str() {
                "status" => crate::release::status(rt)?,
                "resolve" => crate::release::resolve(
                    rt,
                    r.positional(0)?
                        .to_str()
                        .ok_or_else(|| error("invalid channel"))?,
                    r.has("fetch"),
                )?,
                "apply" => {
                    let (receipt, output) = crate::release::apply(
                        rt,
                        r.positional(0)?
                            .to_str()
                            .ok_or_else(|| error("invalid channel"))?,
                        !r.has("no-fetch"),
                        !r.has("tools"),
                    )?;
                    stdout_prefix.extend(output.stdout);
                    stderr.extend(output.stderr);
                    receipt
                }
                "promote" => crate::release::promote(
                    rt,
                    r.text("commit", "")?,
                    r.text("version", "")?,
                    r.text("expected-manifest-digest", "")?,
                )?,
                _ => return Err(error("unknown release action")),
            };
            if value
                .opt("ready")
                .or(value.opt("applied"))
                .is_some_and(|v| !v.truth())
            {
                code = 3
            }
            value
        }
        Mode::Capsule => match r.action.as_str() {
            "export" => crate::capsule::export(rt, path_option(rt, r, "output")?.as_deref())?,
            "verify" => crate::capsule::verify(rt, r.positional(0)?)?,
            "import" => crate::capsule::import(rt, r.positional(0)?)?,
            "diff" => crate::capsule::diff(
                &parse(&rt.read_user(r.positional(0)?, 1024 * 1024)?)?,
                &parse(&rt.read_user(r.positional(1)?, 1024 * 1024)?)?,
            )?,
            _ => return Err(error("unknown capsule action")),
        },
        Mode::Backend => {
            if r.action == "target" {
                let name = if r.subaction == "list" {
                    ""
                } else {
                    r.positional(0)?
                        .to_str()
                        .ok_or_else(|| error("invalid target name"))?
                };
                let value = match r.subaction.as_str() {
                    "list" => crate::targets::listing(rt)?,
                    "add" => {
                        require(
                            !r.has("scratch-root") || !r.text("scratch-root", "")?.is_empty(),
                            "--scratch-root must be non-empty",
                        )?;
                        crate::targets::update_with_options(
                            rt,
                            name,
                            Some(r.text("ssh-alias", "")?),
                            r.os("workdir")
                                .map(|s| {
                                    s.to_str()
                                        .ok_or_else(|| error("remote workdir must be UTF-8"))
                                })
                                .transpose()?,
                            &strings(&r.all("capability"))?,
                            r.text("scheduler", "none")?,
                            r.text("scratch-root", "")?,
                        )?
                    }
                    "remove" => crate::targets::update(rt, name, None, None)?,
                    "approve" | "revoke" => {
                        crate::targets::approval(rt, name, r.subaction == "approve")?
                    }
                    "status" | "sync" => crate::targets::inspect(
                        rt,
                        name,
                        r.subaction == "sync",
                        r.os("sha")
                            .map(|s| s.to_str().ok_or_else(|| error("invalid SHA")))
                            .transpose()?,
                        r.integer("timeout", if r.subaction == "sync" { 1800 } else { 120 })?,
                    )?,
                    _ => return Err(error("unknown target action")),
                };
                code = if value.text("reason") == "unknown-target" {
                    2
                } else if value.opt("ok").is_some_and(|v| !v.truth()) {
                    3
                } else {
                    0
                };
                value
            } else {
                let profile = r
                    .positional(0)?
                    .to_str()
                    .ok_or_else(|| error("invalid profile"))?;
                if let Some(target) = r.os("target") {
                    require(profile == "remote", "--target requires the remote backend")?;
                    require(
                        r.action != "run",
                        "SSH target execution is not yet supported",
                    )
                    .map_err(|_| crate::status(3, "SSH target execution is not yet supported"))?;
                    code = 3;
                    crate::targets::check(
                        rt,
                        target
                            .to_str()
                            .ok_or_else(|| error("invalid target name"))?,
                    )?
                } else {
                    match r.action.as_str() {
                        "describe" => crate::backend::describe(profile)?,
                        "check" => {
                            let value = crate::backend::check(
                                rt,
                                profile,
                                r.os("image").unwrap_or(OsStr::new("")),
                                r.os("adapter").unwrap_or(OsStr::new("")),
                            )?;
                            if !value.get("ready").truth() {
                                code = 3
                            }
                            value
                        }
                        "run" => {
                            let request = crate::backend::request(rt, r, profile, 1200)?;
                            let result = crate::backend::execute(rt, &request)?;
                            code = result.exit;
                            result.receipt
                        }
                        _ => return Err(error("unknown backend action")),
                    }
                }
            }
        }
        Mode::Experiment => match r.action.as_str() {
            "template" => {
                let value = from_native(&oms_experiment::template());
                if let Some(p) = path_option(rt, r, "output")? {
                    rt.write_json_output(&p, &value)?;
                }
                value
            }
            "validate" | "compare" => {
                let mut args = vec![
                    r.action.clone().into(),
                    "--repo".into(),
                    rt.repo.as_os_str().to_owned(),
                ];
                if r.pretty {
                    args.push("--pretty".into())
                }
                for p in &r.positionals {
                    let value = rt.load_argument(p)?;
                    args.push(String::from_utf8(canonical(&value)?).map_err(error)?.into());
                }
                return Ok(Response::native(oms_command_experiment::run(
                    &args,
                    &rt.repo,
                    rt.interrupted,
                )));
            }
            "register" => crate::experiment::register(
                rt,
                &rt.load_argument(r.positional(0)?)?,
                r.text("id", "")?,
            )?,
            "show" => crate::experiment::load(
                rt,
                r.positional(0)?
                    .to_str()
                    .ok_or_else(|| error("invalid experiment id"))?,
            )?,
            "run" => {
                let (value, rc) = crate::experiment::record_run(rt, r)?;
                code = rc;
                value
            }
            "reconcile" => {
                let (value, rc) = crate::experiment::reconcile(rt, r)?;
                code = rc;
                value
            }
            "launch" => return crate::experiment::launch(rt, r),
            "summarize" => crate::experiment::summarize(rt, r.text("id", "")?)?,
            "evaluate" => crate::experiment::evaluate(
                &rt.load_argument(r.positional(0)?)?,
                &rt.load_argument(r.positional(1)?)?,
            )?,
            "invariants" => {
                crate::experiment::invariants(rt, r, &rt.load_argument(r.positional(0)?)?)?
            }
            "sweep" => {
                let value = crate::experiment::sweep(&rt.load_argument(r.positional(0)?)?)?;
                let emit = r.text("emit", "")?;
                if !emit.is_empty() {
                    let mut response = Response::text(crate::experiment::emit_sweep(&value, emit)?);
                    for note in value.get("notes").strings()? {
                        response.stderr.extend(format!("note: {note}\n").as_bytes());
                    }
                    return Ok(response);
                }
                value
            }
            _ => return Err(error("unknown experiment action")),
        },
        Mode::Benchmark => match r.action.as_str() {
            "show" => crate::benchmark::snapshot(rt)?,
            "snapshot" => {
                let value = crate::benchmark::snapshot(rt)?;
                rt.append(Path::new(".oms/runtime/effectiveness.jsonl"), &value)?;
                value
            }
            "record" => {
                let mut metrics = empty();
                for k in crate::benchmark::METRICS {
                    metrics.set(k, number(r.integer(&k.replace('_', "-"), 0)?))?;
                }
                crate::benchmark::record(
                    rt,
                    r.text("task-id", "")?,
                    r.text("status", "")?,
                    &metrics,
                    r.text("note", "")?,
                )?
            }
            "compare" => crate::benchmark::compare(
                &rt.load_argument(r.positional(0)?)?,
                &rt.load_argument(r.positional(1)?)?,
            )?,
            _ => return Err(error("unknown benchmark action")),
        },
        Mode::Doctor => {
            let envelope = crate::projection::envelope(rt)?;
            let profile = crate::profiles::current(rt)?;
            let release = crate::release::status(rt)?;
            let mut issues = vec![];
            if !profile.get("check").get("ready").truth() {
                issues.push("configured capability profile is not ready")
            }
            if !envelope.get("criteria").truth() {
                issues.push("no acceptance criteria are projected")
            }
            if envelope.get("failures").truth() {
                issues.push("unresolved failures exist")
            }
            let stable = release.get("stable");
            if !stable.text("configured_ref").is_empty() && !stable.get("ready").truth() {
                issues.push("configured stable channel ref is unavailable locally")
            }
            if r.has("strict") && !issues.is_empty() {
                code = 3
            }
            object([
                ("schema", number(1)),
                ("ok", b(issues.is_empty())),
                ("issues", list(issues)),
                ("profile", profile),
                ("state_digest", envelope.get("state_digest").clone()),
                ("release", release),
            ])
        }
        _ => return Err(error("unknown command")),
    };
    if r.mode == Mode::Experiment && value.text("verdict") == "not_supported" {
        code = 3
    }
    let mut response = Response::json(&value, r.pretty, code)?;
    stdout_prefix.extend(response.stdout);
    response.stdout = stdout_prefix;
    response.stderr = stderr;
    Ok(response)
}
