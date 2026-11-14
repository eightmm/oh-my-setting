//! Explicit local recipes. Durable running receipts make interrupted effects uncertain.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, error, helpers, json, json_output, require, storage,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) const HELP: &str = "Usage: oms recipe [--repo PATH] validate|plan|run FILE [--input NAME=VALUE]... [--overlay FILE] [--execute]\n       oms recipe resume ID --file FILE [--execute] [--retry-failed] [--retry-uncertain]\n       oms recipe show ID\n       oms recipe history [--id ID] [--limit N]\n       oms recipe workspace plan|run FILE ACTION [--input NAME=VALUE]... [--port NAME=N]... [--execute]\n       oms recipe pack inspect|import DIRECTORY [--digest SHA] [--resource NAME]...\n       oms recipe pack list\n       oms recipe workset put FILE [--replace]\n       oms recipe workset show ID\n       oms recipe workset list\n       oms recipe trigger add FILE\n       oms recipe trigger list|requests|tick|event|enable|disable|plan|cancel|run ...\nHost execution requires --execute. Membership and stored requests grant no authority.\n";
const BASE: &str = ".oms/recipes";
const MAX_ATTEMPTS: usize = 256;
const MAX_RECORDS: usize = 1024;
#[path = "recipe_catalog.rs"]
mod catalog;
fn recipe_parent_only() -> Result<()> {
    crate::parent_only()?;
    require(
        std::env::var("OMS_RECIPE_PROCESS").as_deref() != Ok("1"),
        "a harness child or recipe process may only use read-only recipe actions",
    )
}
struct Loaded {
    definition: Value,
    resources: Value,
    source: Value,
}
fn restricted_name(raw: &str) -> bool {
    regex::Regex::new(concat!(
        r"(?i)(?:to",
        r"ken|sec",
        r"ret|pass",
        r"word|api[_-]?key|creden",
        r"tial|private[_-]?key|auth[_-]?key)"
    ))
    .expect("static expression")
    .is_match(raw)
}

fn record_path(group: &str, id: &str) -> Result<PathBuf> {
    helpers::identifier(id)?;
    Ok(Path::new(BASE).join(group).join(format!("{id}.json")))
}

fn load(ctx: &Context<'_>, path: &Path) -> Result<Value> {
    load_bounded(ctx, path, storage::MAX_FILE)
}
fn definition(ctx: &Context<'_>, path: &Path) -> Result<Value> {
    let value = load_bounded(ctx, path, 256 * 1024)?;
    helpers::scrub(&value)?;
    Ok(value)
}
fn load_bounded(ctx: &Context<'_>, path: &Path, limit: usize) -> Result<Value> {
    let raw = storage::read(&ctx.repo, path, limit, false)?;
    let value = json::parse(&raw)?;
    value.object()?;
    Ok(value)
}

fn records(ctx: &Context<'_>, group: &str) -> Result<Vec<Value>> {
    let directory = ctx.repo.join(BASE).join(group);
    let mut rows = Vec::new();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(rows),
        Err(e) => return Err(error(2, e)),
    };
    let mut count = 0;
    let mut bytes = 0u64;
    for entry in entries {
        let entry = entry.map_err(|e| error(2, e))?;
        count += 1;
        require(count <= 1024, "recipe state scan exceeds 1024 entries")?;
        if !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        let meta = entry.path().symlink_metadata().map_err(|e| error(2, e))?;
        require(
            meta.is_file(),
            "recipe records must be non-symlink regular files",
        )?;
        bytes = bytes.saturating_add(meta.len());
        require(
            bytes <= storage::MAX_STREAM as u64,
            "recipe collection exceeds 32 MiB",
        )?;
        rows.push(load(ctx, &entry.path())?);
    }
    rows.sort_by(|a, b| a.text("id").cmp(b.text("id")));
    Ok(rows)
}

fn fields(value: &Value, needed: &[&str], optional: &[&str]) -> Result<()> {
    json::keys(value, needed, optional)
}

fn bounded(value: &Value, default: i128, low: i128, high: i128) -> Result<i128> {
    let n = if matches!(value, Value::Null) {
        default
    } else {
        value.int()?
    };
    require(
        (low..=high).contains(&n),
        "recipe integer outside permitted range",
    )?;
    Ok(n)
}

fn validate(value: &Value) -> Result<()> {
    fields(
        value,
        &["schema", "kind", "id", "commands"],
        &[
            "description",
            "inputs",
            "environment",
            "outputs",
            "steps",
            "setup",
            "actions",
            "ports",
            "companions",
        ],
    )?;
    require(
        value.get("schema").is_int(1) && ["recipe", "workspace"].contains(&value.text("kind")),
        "expected recipe/workspace schema 1",
    )?;
    helpers::identifier(value.text("id"))?;
    validate_outputs(value.get("outputs"))?;
    if let Some(inputs_spec) = value.opt("inputs") {
        require(
            inputs_spec.object()?.len() <= 64,
            "too many input specifications",
        )?;
        for (key, spec) in inputs_spec.object()? {
            let name = std::str::from_utf8(key).map_err(|_| error(2, "invalid input name"))?;
            require(!restricted_name(name), "sensitive input names unsupported")?;
            fields(
                spec,
                &["type"],
                &["default", "required", "choices", "min", "max", "max_length"],
            )?;
            require(
                ["string", "path", "enum", "integer", "number", "boolean"]
                    .contains(&spec.text("type")),
                "unsupported input type",
            )?;
            if let Some(required) = spec.opt("required") {
                require(
                    matches!(required, Value::Bool(_)),
                    "input required must be boolean",
                )?;
                if required == &Value::Bool(false) {
                    require(
                        spec.opt("default").is_some(),
                        "optional input requires a default",
                    )?;
                }
            }
            if spec.text("type") == "enum" {
                let choices = spec.get("choices").strings()?;
                require(
                    !choices.is_empty()
                        && choices.len() <= 64
                        && choices.iter().copied().collect::<BTreeSet<_>>().len() == choices.len(),
                    "invalid enum choices",
                )?;
            } else {
                require(
                    spec.opt("choices").is_none(),
                    "choices applies only to enum inputs",
                )?;
            }
            if let Some(bound) = spec.opt("max_length") {
                bounded(bound, 0, 1, 4096)?;
            }
            for field in ["min", "max"] {
                if let Some(bound) = spec.opt(field) {
                    require(
                        ["integer", "number"].contains(&spec.text("type")),
                        "bounds require numeric input",
                    )?;
                    let Value::Number(raw) = bound else {
                        return Err(error(2, "numeric bound required"));
                    };
                    require(
                        raw.parse::<f64>().is_ok_and(f64::is_finite),
                        "numeric bound must be finite",
                    )?;
                }
            }
            if spec.opt("default").is_some() {
                let single = Value::Object(vec![(key.clone(), spec.clone())]);
                inputs(&single, &json::empty())?;
            }
        }
    }
    require(
        value.get("commands").object()?.len() <= 64,
        "too many commands",
    )?;
    for (name, cmd) in value.get("commands").object()? {
        helpers::identifier(
            std::str::from_utf8(name).map_err(|_| error(2, "invalid command name"))?,
        )?;
        fields(
            cmd,
            &["argv"],
            &[
                "cwd",
                "environment",
                "depends",
                "timeout_seconds",
                "max_output_bytes",
            ],
        )?;
        let argv = cmd.get("argv").strings()?;
        require(
            !argv.is_empty() && argv.len() <= 128,
            "command argv must contain 1..128 entries",
        )?;
        require(
            !argv[0].contains("${input.") && !argv[0].contains("${port."),
            "executable cannot depend on user inputs",
        )?;
        for arg in &argv {
            template(arg)?;
            require(
                arg.len() <= 4096 && !arg.chars().any(char::is_control),
                "invalid recipe argument",
            )?;
        }
        let interpreter = Path::new(argv[0])
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if [
            "sh", "bash", "dash", "zsh", "ksh", "python", "python3", "node", "ruby", "perl",
        ]
        .contains(&interpreter)
        {
            require(
                !argv
                    .windows(2)
                    .any(|a| ["-c", "-e", "--eval"].contains(&a[0]) && a[1].contains("${")),
                "interpreter code cannot interpolate recipe inputs",
            )?;
        }
        if let Some(cwd) = cmd.opt("cwd") {
            data_template(cwd.str()?, true)?;
        }
        if let Some(deps) = cmd.opt("depends") {
            require(deps.array()?.len() <= 64, "too many dependencies")?;
            for path in deps.strings()? {
                data_template(path, false)?;
            }
        }
        bounded(cmd.get("timeout_seconds"), 1200, 1, 86400)?;
        bounded(
            cmd.get("max_output_bytes"),
            1024 * 1024,
            1024,
            16 * 1024 * 1024,
        )?;
        if let Some(env) = cmd.opt("environment") {
            environment(env, &json::empty(), false)?;
        }
    }
    if let Some(env) = value.opt("environment") {
        environment(env, &json::empty(), false)?;
    }
    if value.text("kind") == "recipe" {
        steps(value.get("steps"), value.get("commands"))?;
    } else {
        let mut count = 0;
        if let Some(setup) = value.opt("setup") {
            steps(setup, value.get("commands"))?;
            count += 1;
        }
        if let Some(actions) = value.opt("actions") {
            for (name, action) in actions.object()? {
                require(name != b"setup", "setup is reserved")?;
                fields(action, &["steps"], &["outputs"])?;
                validate_outputs(action.get("outputs"))?;
                steps(action.get("steps"), value.get("commands"))?;
                count += 1;
            }
        }
        require(count > 0, "workspace requires setup or actions")?;
    }
    Ok(())
}

fn steps(value: &Value, commands: &Value) -> Result<()> {
    let rows = value.array()?;
    require(
        !rows.is_empty() && rows.len() <= 64,
        "steps must contain 1..64 entries",
    )?;
    let mut ids = BTreeSet::new();
    for row in rows {
        fields(row, &["id", "command"], &["outputs"])?;
        validate_outputs(row.get("outputs"))?;
        helpers::identifier(row.text("id"))?;
        require(
            ids.insert(row.text("id")) && commands.opt(row.text("command")).is_some(),
            "duplicate step or undefined command",
        )?;
    }
    Ok(())
}

fn pairs(values: &[String]) -> Result<Value> {
    let mut result = json::empty();
    for value in values {
        let (key, value) = value
            .split_once('=')
            .ok_or_else(|| error(2, "expected NAME=VALUE"))?;
        require(result.opt(key).is_none(), "duplicate binding")?;
        helpers::identifier(key)?;
        result.set(key, string(value))?;
    }
    Ok(result)
}

fn inputs(specs: &Value, supplied: &Value) -> Result<Value> {
    let empty = json::empty();
    let specs = if matches!(specs, Value::Null) {
        &empty
    } else {
        specs
    };
    require(specs.object()?.len() <= 64, "too many inputs")?;
    for (key, _) in supplied.object()? {
        require(
            specs.opt(std::str::from_utf8(key).unwrap_or("")).is_some(),
            "unknown recipe input",
        )?;
    }
    let mut result = json::empty();
    for (key, spec) in specs.object()? {
        let key = std::str::from_utf8(key).map_err(|_| error(2, "invalid input name"))?;
        fields(
            spec,
            &["type"],
            &["default", "required", "choices", "min", "max", "max_length"],
        )?;
        let supplied_value = supplied.opt(key);
        let mut value = supplied_value
            .cloned()
            .or_else(|| spec.opt("default").cloned())
            .ok_or_else(|| error(2, format!("missing recipe input {key}")))?;
        if matches!(&value, Value::String(_))
            && supplied_value.is_some()
            && ["integer", "number", "boolean"].contains(&spec.text("type"))
        {
            value = json::parse(value.str()?.as_bytes())?;
        }
        match spec.text("type") {
            "string" | "path" | "enum" => {
                let text = value.str()?;
                require(
                    text.len() <= bounded(spec.get("max_length"), 4096, 1, 4096)? as usize
                        && !text.chars().any(char::is_control),
                    "input exceeds text bounds",
                )?;
                if spec.text("type") == "path" && text != "." {
                    helpers::relative(text)?;
                }
                if spec.text("type") == "enum" {
                    require(
                        spec.get("choices").array()?.contains(&value),
                        "input is outside enum choices",
                    )?;
                }
            }
            "integer" | "number" => {
                let n = if spec.text("type") == "integer" {
                    value.int()? as f64
                } else {
                    match &value {
                        Value::Number(s) => {
                            s.parse::<f64>().map_err(|_| error(2, "invalid number"))?
                        }
                        _ => return Err(error(2, "input must be numeric")),
                    }
                };
                require(n.is_finite(), "input must be finite")?;
                for field in ["min", "max"] {
                    if let Value::Number(raw) = spec.get(field) {
                        let bound = raw
                            .parse::<f64>()
                            .map_err(|_| error(2, "invalid numeric bound"))?;
                        require(
                            bound.is_finite()
                                && if field == "min" {
                                    n >= bound
                                } else {
                                    n <= bound
                                },
                            "input outside numeric bounds",
                        )?;
                    }
                }
            }
            "boolean" => require(matches!(value, Value::Bool(_)), "boolean input required")?,
            _ => return Err(error(2, "unsupported input type")),
        }
        helpers::scrub(&value)?;
        result.set(key, value)?;
    }
    Ok(result)
}

fn expand(raw: &str, bindings: &Value) -> Result<String> {
    let re = regex::Regex::new(r"\$\{(input|port|resource)\.([A-Za-z][A-Za-z0-9_-]{0,63})\}")
        .expect("static expression");
    let mut result = String::new();
    let mut offset = 0;
    for matched in re.captures_iter(raw) {
        let m = matched.get(0).expect("whole match");
        result.push_str(&raw[offset..m.start()]);
        let value = bindings.get(&matched[1]).get(&matched[2]);
        require(!matches!(value, Value::Null), "undefined interpolation")?;
        result.push_str(&match value {
            Value::String(_) => value.str()?.to_owned(),
            Value::Number(n) => n.clone(),
            Value::Bool(b) => b.to_string(),
            _ => return Err(error(2, "interpolation must be scalar")),
        });
        offset = m.end();
    }
    result.push_str(&raw[offset..]);
    require(
        !result.contains("${") && result.len() <= 4096 && !result.chars().any(char::is_control),
        "unsupported or oversized interpolation",
    )?;
    Ok(result)
}

fn environment(
    specs: &Value,
    bindings: &Value,
    resolve: bool,
) -> Result<BTreeMap<OsString, OsString>> {
    let mut result = BTreeMap::new();
    require(specs.object()?.len() <= 64, "too many environment entries")?;
    let re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$").expect("static expression");
    for (name, entry) in specs.object()? {
        let name = std::str::from_utf8(name).map_err(|_| error(2, "invalid environment name"))?;
        require(
            re.is_match(name)
                && !restricted_name(name)
                && !name.starts_with("OMS_")
                && !name.starts_with("LD_")
                && !name.starts_with("DYLD_")
                && ![
                    "PATH",
                    "HOME",
                    "PYTHONPATH",
                    "PYTHONHOME",
                    "BASH_ENV",
                    "ENV",
                    "SHELLOPTS",
                ]
                .contains(&name),
            "environment cannot override execution or authority controls",
        )?;
        fields(entry, &[], &["value", "from_env"])?;
        require(
            entry.object()?.len() == 1,
            "environment entry needs exactly one source",
        )?;
        let value = if let Some(v) = entry.opt("value") {
            if bindings.object()?.is_empty() {
                v.str()?.to_owned()
            } else {
                expand(v.str()?, bindings)?
            }
        } else {
            let reference = entry.get("from_env").str()?;
            require(
                re.is_match(reference) && !restricted_name(reference),
                "invalid environment reference",
            )?;
            if !resolve {
                continue;
            }
            std::env::var(reference)
                .map_err(|_| error(2, "referenced environment is unavailable"))?
        };
        helpers::scrub(&object([(name, string(&value))]))?;
        result.insert(name.into(), value.into());
    }
    Ok(result)
}

fn confined(repo: &Path, path: &str) -> Result<PathBuf> {
    if path == "." {
        return Ok(repo.to_owned());
    }
    let rel = helpers::relative(path)?;
    require(
        !rel.components().any(|c| c.as_os_str() == ".oms"),
        "recipe cwd, dependencies and outputs cannot target OMS state",
    )?;
    let mut at = repo.to_owned();
    for component in rel.components() {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Ok(m) => require(!m.is_symlink(), "recipe data path contains a symlink")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(2, e)),
        }
    }
    Ok(at)
}

fn executable(
    cwd: &Path,
    name: &str,
    search: &std::ffi::OsStr,
    resources: &Value,
) -> Result<PathBuf> {
    let candidate = if name.contains('/') {
        require(
            !Path::new(name).is_absolute()
                || resources.object()?.iter().any(|(_, v)| v.str() == Ok(name)),
            "absolute executable needs a pinned pack resource",
        )?;
        if Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            cwd.join(helpers::relative(name)?)
        }
    } else {
        std::env::split_paths(search)
            .map(|p| cwd.join(p).join(name))
            .find(|p| rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok())
            .ok_or_else(|| error(2, "declared command unavailable"))?
    };
    let path = helpers::absolute(cwd, candidate);
    require(
        path.is_file() && rustix::fs::access(&path, rustix::fs::Access::EXEC_OK).is_ok(),
        "declared command is not executable",
    )?;
    Ok(path)
}

fn file_hash(path: &Path, limit: usize) -> Result<String> {
    let parent = path
        .parent()
        .ok_or_else(|| error(2, "file has no parent"))?;
    Ok(json::sha(storage::read(parent, path, limit, false)?))
}

fn bundle_with(
    ctx: &Context<'_>,
    loaded: &Loaded,
    overlay_arg: &str,
) -> Result<(Value, String, Value)> {
    let mut definition = loaded.definition.clone();
    validate(&definition)?;
    let original = json::hash(&definition)?;
    let mut overlay_digest = Value::Null;
    if !overlay_arg.is_empty() {
        require(
            definition.text("kind") == "recipe",
            "project overlays currently apply to recipes",
        )?;
        let overlay = catalog::source(ctx, overlay_arg)?.definition;
        fields(
            &overlay,
            &["schema", "kind", "base_digest", "steps"],
            &["commands", "outputs"],
        )?;
        require(
            overlay.get("schema").is_int(1)
                && overlay.text("kind") == "recipe-overlay"
                && overlay.text("base_digest") == original,
            "overlay base digest does not match",
        )?;
        overlay_digest = string(json::hash(&overlay)?);
        let replacements = overlay.get("steps").array()?;
        let mut combined = definition.get("steps").array()?.to_vec();
        for row in replacements {
            if let Some(i) = combined.iter().position(|r| r.text("id") == row.text("id")) {
                combined[i] = row.clone();
            } else {
                combined.push(row.clone());
            }
        }
        definition.set("steps", Value::Array(combined))?;
        let mut all = definition.get("outputs").array().unwrap_or(&[]).to_vec();
        all.extend_from_slice(overlay.get("outputs").array().unwrap_or(&[]));
        definition.set("outputs", Value::Array(all))?;
        if let Some(commands) = overlay.opt("commands") {
            let mut merged = definition.get("commands").clone();
            for (key, value) in commands.object()? {
                require(
                    merged.opt(std::str::from_utf8(key).unwrap_or("")).is_none(),
                    "overlay commands must have new names",
                )?;
                merged.set(
                    std::str::from_utf8(key).map_err(|_| error(2, "invalid command name"))?,
                    value.clone(),
                )?;
            }
            definition.set("commands", merged)?;
        }
        validate(&definition)?;
    }
    Ok((definition, original, overlay_digest))
}

fn bundle(ctx: &Context<'_>, loaded: &Loaded, opts: &Options) -> Result<(Value, String, Value)> {
    bundle_with(ctx, loaded, opts.get("overlay"))
}

fn compile(ctx: &Context<'_>, loaded: Loaded, opts: &Options, action: &str) -> Result<Value> {
    let supplied_inputs = pairs(opts.all("input"))?;
    let supplied_ports = pairs(opts.all("port"))?;
    compile_with(
        ctx,
        loaded,
        opts.get("overlay"),
        &supplied_inputs,
        &supplied_ports,
        opts.all("with"),
        action,
    )
}

fn compile_with(
    ctx: &Context<'_>,
    loaded: Loaded,
    overlay_arg: &str,
    supplied_inputs: &Value,
    supplied_ports: &Value,
    with_companions: &[String],
    action: &str,
) -> Result<Value> {
    let (definition, original, overlay_digest) = bundle_with(ctx, &loaded, overlay_arg)?;
    let input = inputs(definition.get("inputs"), supplied_inputs)?;
    let mut ports = definition.opt("ports").cloned().unwrap_or_else(json::empty);
    for (name, value) in supplied_ports.object()? {
        let name = std::str::from_utf8(name).map_err(|_| error(2, "invalid port name"))?;
        require(ports.opt(name).is_some(), "unknown workspace port")?;
        let port = match value {
            Value::String(_) => value
                .str()?
                .parse::<u16>()
                .map_err(|_| error(2, "invalid port number"))?,
            Value::Number(_) => {
                u16::try_from(value.int()?).map_err(|_| error(2, "invalid port number"))?
            }
            _ => return Err(error(2, "invalid port number")),
        };
        ports.set(name, number(port))?;
    }
    let mut used = BTreeSet::new();
    for (_, value) in ports.object()? {
        let port = bounded(value, 0, 1, 65535)?;
        require(used.insert(port), "duplicate workspace port")?;
    }
    let bindings = object([
        ("input", input.clone()),
        ("port", ports.clone()),
        ("resource", loaded.resources.clone()),
    ]);
    let (selected, outputs) = if definition.text("kind") == "recipe" {
        require(
            action.is_empty() && ports.object()?.is_empty(),
            "workspace selection on recipe",
        )?;
        (
            definition.get("steps"),
            definition
                .opt("outputs")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        )
    } else if action == "setup" {
        (
            definition.get("setup"),
            definition
                .opt("outputs")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        )
    } else {
        let selected = definition
            .get("actions")
            .opt(action)
            .ok_or_else(|| error(2, "unknown workspace action"))?;
        let mut all = definition.get("outputs").array().unwrap_or(&[]).to_vec();
        all.extend_from_slice(selected.get("outputs").array().unwrap_or(&[]));
        (selected.get("steps"), Value::Array(all))
    };
    let mut base_env: BTreeMap<OsString, OsString> = BTreeMap::new();
    for key in [
        "PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "USER", "LOGNAME",
    ] {
        if let Some(value) = std::env::var_os(key) {
            base_env.insert(key.into(), value);
        }
    }
    if let Some(env) = definition.opt("environment") {
        base_env.extend(environment(env, &bindings, true)?);
    }
    let mut planned = Vec::new();
    let mut companions = Vec::new();
    let mut budget = 0;
    let step_count = selected.array()?.len();
    let mut work = selected.array()?.to_vec();
    let mut sidecars = Vec::new();
    let mut unique = BTreeSet::new();
    require(with_companions.len() <= 8, "too many workspace companions")?;
    for name in with_companions {
        require(unique.insert(name), "duplicate workspace companion")?;
        let row = definition
            .get("companions")
            .opt(name)
            .ok_or_else(|| error(2, "undefined workspace companion"))?;
        fields(row, &["command"], &["ready_port", "startup_seconds"])?;
        require(
            definition
                .get("commands")
                .opt(row.text("command"))
                .is_some(),
            "companion command undefined",
        )?;
        let port = if let Some(name) = row.opt("ready_port") {
            ports
                .opt(name.str()?)
                .cloned()
                .ok_or_else(|| error(2, "companion port undefined"))?
        } else {
            Value::Null
        };
        let startup = bounded(row.get("startup_seconds"), 10, 1, 60)?;
        sidecars.push(object([
            ("id", string(name)),
            ("ready_port", port),
            ("startup_seconds", number(startup)),
        ]));
        work.push(object([
            ("id", string(name)),
            ("command", row.get("command").clone()),
            ("outputs", Value::Array(vec![])),
        ]));
    }
    for (index, row) in work.iter().enumerate() {
        let raw = definition.get("commands").get(row.text("command"));
        let argv = raw
            .get("argv")
            .strings()?
            .into_iter()
            .map(|s| expand(s, &bindings))
            .collect::<Result<Vec<_>>>()?;
        let cwd = confined(
            &ctx.repo,
            &expand(
                raw.opt("cwd").and_then(|s| s.str().ok()).unwrap_or("."),
                &bindings,
            )?,
        )?;
        require(cwd.is_dir(), "command cwd does not exist")?;

        let mut env = base_env.clone();
        if let Some(entries) = raw.opt("environment") {
            env.extend(environment(entries, &bindings, true)?);
        }
        env.insert("OMS_RECIPE_PROCESS".into(), "1".into());
        for (key, value) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("OMS_HARNESS_") {
                env.insert(key, value);
            }
        }
        let search = env
            .get(std::ffi::OsStr::new("PATH"))
            .cloned()
            .unwrap_or_else(|| "/usr/bin:/bin".into());
        let binary = executable(&cwd, &argv[0], &search, &loaded.resources)?;
        let digest = file_hash(
            &binary.canonicalize().map_err(|e| error(2, e))?,
            64 * 1024 * 1024,
        )?;
        let environment = Value::Object(
            env.iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().as_bytes().to_vec(),
                        string(v.to_string_lossy()),
                    )
                })
                .collect(),
        );
        let mut dependencies = Vec::new();
        for path in raw.get("depends").array().unwrap_or(&[]) {
            let relative = expand(path.str()?, &bindings)?;
            let path = confined(&ctx.repo, &relative)?;
            dependencies.push(object([
                ("path", string(relative)),
                ("sha256", string(file_hash(&path, 16 * 1024 * 1024)?)),
            ]));
        }
        let timeout = bounded(raw.get("timeout_seconds"), 1200, 1, 86400)?;
        budget += if index < step_count {
            timeout
        } else {
            sidecars[index - step_count].get("startup_seconds").int()?
        };
        let command = object([
            ("name", string(row.text("command"))),
            ("argv", json::list(argv.iter().map(String::as_str))),
            ("executable", string(binary.to_string_lossy())),
            ("executable_digest", string(digest)),
            (
                "executable_path_digest",
                string(json::hash(&string(binary.to_string_lossy()))?),
            ),
            (
                "cwd",
                string({
                    let rel = cwd.strip_prefix(&ctx.repo).unwrap_or(&cwd);
                    if rel.as_os_str().is_empty() {
                        std::borrow::Cow::Borrowed(".")
                    } else {
                        rel.to_string_lossy()
                    }
                }),
            ),
            ("environment_digest", string(json::hash(&environment)?)),
            ("environment", environment),
            ("dependencies", Value::Array(dependencies)),
            (
                "command_digest",
                string(json::hash(&json::list(argv.iter().map(String::as_str)))?),
            ),
            ("timeout_seconds", number(timeout)),
            (
                "max_output_bytes",
                number(bounded(
                    raw.get("max_output_bytes"),
                    1024 * 1024,
                    1024,
                    16 * 1024 * 1024,
                )?),
            ),
        ]);
        if index < step_count {
            planned.push(object([
                ("id", row.get("id").clone()),
                ("command", command),
                (
                    "outputs",
                    expand_outputs(ctx, row.get("outputs"), &bindings)?,
                ),
            ]));
        } else {
            let mut sidecar = sidecars[index - step_count].clone();
            sidecar.set("command", command)?;
            companions.push(sidecar);
        }
    }
    require(budget <= 86400, "invocation budget exceeds 86400 seconds")?;
    let mut plan = object([
        ("schema", number(1)),
        ("id", definition.get("id").clone()),
        ("kind", definition.get("kind").clone()),
        ("action", string(action)),
        ("definition_digest", string(original)),
        ("overlay_digest", overlay_digest),
        ("effective_digest", string(json::hash(&definition)?)),
        ("inputs", input),
        ("ports", ports),
        ("steps", Value::Array(planned)),
        ("companions", Value::Array(companions)),
        ("outputs", expand_outputs(ctx, &outputs, &bindings)?),
        ("source", loaded.source),
    ]);
    plan.set("plan_digest", string(json::hash(&plan_signature(&plan)?)?))?;
    Ok(plan)
}

fn public_plan(value: &Value) -> Result<Value> {
    let mut value = value.clone();
    for key in ["steps", "companions"] {
        let mut steps = value.get(key).array()?.to_vec();
        for row in &mut steps {
            let mut cmd = row.get("command").clone();
            let env = cmd.remove("environment").unwrap_or_else(json::empty);
            cmd.remove("executable");
            cmd.set(
                "environment_names",
                json::list(
                    env.object()?
                        .iter()
                        .map(|(k, _)| std::str::from_utf8(k).unwrap_or("")),
                ),
            )?;
            row.set("command", cmd)?;
        }
        value.set(key, Value::Array(steps))?;
    }
    value.set(
        "execution",
        string("explicit --execute required; supervised host process, not a sandbox"),
    )?;
    Ok(value)
}

fn output_entry(repo: &Path, requirement: &Value) -> Result<Option<Value>> {
    let rel = requirement.text("path");
    let path = confined(repo, rel)?;
    if !path.exists() {
        return Ok(None);
    }
    let limit = bounded(
        requirement.get("max_bytes"),
        16 * 1024 * 1024,
        1,
        64 * 1024 * 1024,
    )? as usize;
    let data = storage::read(repo, &path, limit, false)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| error(2, e))?;
    let mtime_ns = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
    Ok(Some(object([
        ("path", string(rel)),
        ("bytes", number(data.len())),
        ("sha256", string(json::sha(&data))),
        ("mtime_ns", number(mtime_ns)),
    ])))
}

fn snapshot_outputs(repo: &Path, outputs: &Value) -> Result<Value> {
    let mut result = json::empty();
    for req in outputs.array().unwrap_or(&[]) {
        let entry = output_entry(repo, req)?.unwrap_or(Value::Null);
        result.set(req.text("path"), entry)?;
    }
    Ok(result)
}

fn verify_outputs(repo: &Path, outputs: &Value, before: &Value) -> Result<Value> {
    let mut result = Vec::new();
    for req in outputs.array().unwrap_or(&[]) {
        let rel = req.text("path");
        let entry = output_entry(repo, req)?
            .ok_or_else(|| error(2, format!("required output is missing: {rel}")))?;
        let bytes = entry.get("bytes").int()?;
        let min_bytes = bounded(req.get("min_bytes"), 0, 0, 64 * 1024 * 1024)?;
        require(
            bytes >= min_bytes,
            format!("required output is too small: {rel}"),
        )?;
        if let Some(expected) = req.opt("sha256")
            && !matches!(expected, Value::Null)
        {
            require(
                entry.text("sha256") == expected.str()?,
                format!("required output digest does not match: {rel}"),
            )?;
        }
        let fresh = match req.opt("fresh") {
            None | Some(Value::Null) => true,
            Some(v) => v.truth(),
        };
        if fresh && before.opt(rel) == Some(&entry) {
            return Err(error(
                2,
                format!("required output was not refreshed: {rel}"),
            ));
        }
        result.push(object([
            ("path", entry.get("path").clone()),
            ("bytes", entry.get("bytes").clone()),
            ("sha256", entry.get("sha256").clone()),
        ]));
    }
    Ok(Value::Array(result))
}

fn verify_completed(repo: &Path, run: &Value, plan: &Value) -> Result<()> {
    let run_steps = run.get("steps").object()?;
    let plan_steps = plan.get("steps").array()?;
    let run_keys: BTreeSet<&[u8]> = run_steps.iter().map(|(k, _)| k.as_slice()).collect();
    let plan_keys: BTreeSet<&[u8]> = plan_steps.iter().map(|s| s.text("id").as_bytes()).collect();
    require(
        run_keys == plan_keys,
        "run steps do not match the current definition",
    )?;
    for step in plan_steps {
        let rec = run.get("steps").get(step.text("id"));
        require(
            ["pending", "running", "succeeded", "failed", "interrupted"]
                .contains(&rec.text("status")),
            "invalid stored step status",
        )?;
        let attempts = rec.get("attempts").array()?;
        require(
            attempts.len() <= MAX_ATTEMPTS,
            "invalid or exhausted stored attempts",
        )?;
        if rec.text("status") != "succeeded" {
            continue;
        }
        let last = attempts
            .last()
            .filter(|a| a.text("status") == "succeeded")
            .ok_or_else(|| error(2, "completed step is missing a successful receipt"))?;
        let evidence = last.get("outputs").array()?;
        let ev_paths: BTreeSet<&str> = evidence.iter().map(|i| i.text("path")).collect();
        let reqs = step.get("outputs").array()?;
        let req_paths: BTreeSet<&str> = reqs.iter().map(|i| i.text("path")).collect();
        require(
            ev_paths == req_paths,
            "completed step output evidence is incomplete",
        )?;
        for req in reqs {
            let rel = req.text("path");
            let actual = output_entry(repo, req)?;
            let old = evidence
                .iter()
                .find(|i| i.text("path") == rel)
                .ok_or_else(|| error(2, "completed step output evidence is incomplete"))?;
            let unchanged = actual.as_ref().is_some_and(|a| {
                a.text("sha256") == old.text("sha256") && a.get("bytes") == old.get("bytes")
            });
            require(
                unchanged,
                format!("completed output changed; start a new run: {rel}"),
            )?;
        }
    }
    Ok(())
}

fn verify_command_inputs(repo: &Path, command: &Value) -> Result<()> {
    let exe_real = Path::new(command.text("executable"))
        .canonicalize()
        .map_err(|_| error(2, "declared executable missing after planning"))?;
    let exe_digest = file_hash(&exe_real, 64 * 1024 * 1024)?;
    require(
        exe_digest == command.text("executable_digest"),
        "declared executable changed after planning",
    )?;
    for dep in command.get("dependencies").array()? {
        let path = confined(repo, dep.text("path"))?;
        let digest = file_hash(&path, 16 * 1024 * 1024)?;
        require(
            digest == dep.text("sha256"),
            "declared command dependency changed after planning",
        )?;
    }
    Ok(())
}

fn command_env(command: &Value) -> Result<BTreeMap<OsString, OsString>> {
    let mut env = BTreeMap::new();
    for (k, v) in command.get("environment").object()? {
        let key = std::str::from_utf8(k).map_err(|_| error(2, "invalid environment name"))?;
        env.insert(OsString::from(key), OsString::from(v.str()?));
    }
    Ok(env)
}

fn command_argv(command: &Value) -> Result<Vec<OsString>> {
    let raw = command.get("argv").strings()?;
    require(!raw.is_empty(), "command argv is empty")?;
    let mut argv = Vec::with_capacity(raw.len());
    argv.push(OsString::from(command.text("executable")));
    for arg in &raw[1..] {
        argv.push(OsString::from(*arg));
    }
    Ok(argv)
}

fn pid_alive(pid: &Value) -> bool {
    let Ok(raw) = pid.int() else {
        return false;
    };
    let Ok(raw) = i32::try_from(raw) else {
        return false;
    };
    if raw <= 1 {
        return false;
    }
    rustix::process::Pid::from_raw(raw).is_some_and(|p| {
        matches!(
            rustix::process::test_kill_process(p),
            Ok(()) | Err(rustix::io::Errno::PERM)
        )
    })
}

fn obj_get_mut<'a>(val: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match val {
        Value::Object(items) => items
            .iter_mut()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v),
        _ => None,
    }
}

fn obj_items_mut(val: &mut Value) -> Option<&mut Vec<(Vec<u8>, Value)>> {
    match val {
        Value::Object(items) => Some(items),
        _ => None,
    }
}

fn stop_child(child: &mut Child) {
    if let Ok(raw) = i32::try_from(child.id())
        && let Some(pgid) = rustix::process::Pid::from_raw(raw)
    {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::TERM);
    } else {
        let _ = child.kill();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Ok(raw) = i32::try_from(child.id())
        && let Some(pgid) = rustix::process::Pid::from_raw(raw)
    {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
    } else {
        let _ = child.kill();
    }
    let _ = child.wait();
}

struct CompanionGuard {
    processes: Mutex<Vec<Child>>,
}
impl CompanionGuard {
    fn new() -> Self {
        Self {
            processes: Mutex::new(Vec::new()),
        }
    }
    fn stop_all(&self) {
        let mut list = self.processes.lock().expect("companion lock");
        for child in list.iter_mut().rev() {
            stop_child(child);
        }
        list.clear();
    }
}
impl Drop for CompanionGuard {
    fn drop(&mut self) {
        if let Ok(list) = self.processes.get_mut() {
            for child in list.iter_mut().rev() {
                stop_child(child);
            }
        }
    }
}

struct ConsoleCollector {
    bytes: usize,
    hasher: Sha256,
    tail: Vec<u8>,
    sensitive: bool,
}
impl ConsoleCollector {
    fn new() -> Self {
        Self {
            bytes: 0,
            hasher: Sha256::new(),
            tail: Vec::new(),
            sensitive: false,
        }
    }
    fn digest_hex(&self) -> String {
        format!("{:x}", self.hasher.clone().finalize())
    }
}
impl std::io::Write for ConsoleCollector {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        self.hasher.update(buf);
        if !self.sensitive && !buf.is_empty() {
            let mut window = Vec::with_capacity(self.tail.len().saturating_add(buf.len()));
            window.extend_from_slice(&self.tail);
            window.extend_from_slice(buf);
            if oms_scrub::scan(&window, &[]).is_ok_and(|hits| !hits.is_empty()) {
                self.sensitive = true;
            }
            let keep = window.len().saturating_sub(512);
            self.tail.clear();
            self.tail.extend_from_slice(&window[keep..]);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sanitize_failure_reason(raw: &str) -> String {
    let line = raw
        .split(['\n', '\r'])
        .next()
        .unwrap_or("")
        .chars()
        .take(500)
        .collect::<String>();
    let re = regex::Regex::new(r"(?:/[A-Za-z0-9._-]+){2,}").expect("static path pattern");
    let redacted = re.replace_all(&line, "[path omitted]").into_owned();
    if oms_scrub::scan(redacted.as_bytes(), &[]).is_ok_and(|h| h.is_empty()) {
        redacted
    } else {
        "local process or filesystem operation failed".into()
    }
}

struct StepOutcome {
    code: i32,
    reason: &'static str,
    duration_seconds: f64,
    console_bytes: usize,
    console_sha256: String,
    sensitive: bool,
    interrupted: bool,
}

fn run_step_command(
    ctx: &Context<'_>,
    command: &Value,
    companions: &CompanionGuard,
    has_companions: bool,
    run_path: &Path,
    run: &mut Value,
    step_id: &str,
) -> Result<StepOutcome> {
    verify_command_inputs(&ctx.repo, command)?;
    let cwd = confined(&ctx.repo, command.text("cwd"))?;
    let argv = command_argv(command)?;
    let env = command_env(command)?;
    let timeout_secs = u64::try_from(command.get("timeout_seconds").int()?)
        .map_err(|_| error(2, "invalid step timeout"))?;
    let max_output = usize::try_from(command.get("max_output_bytes").int()?)
        .map_err(|_| error(2, "invalid step max_output_bytes"))?;
    let policy = oms_command_runs::process::ProcessPolicy {
        env_clear: true,
        env,
        null_stdin: true,
        merged: true,
        ..Default::default()
    };

    let companion_exited = AtomicBool::new(false);
    let stop_watch = AtomicBool::new(false);
    let step_interrupted = AtomicBool::new(false);
    let mut collector = ConsoleCollector::new();
    let mut spawned_pid: Option<u32> = None;
    let mut pid_persist_err: Option<crate::Error> = None;

    let started = Instant::now();
    let exec_result = std::thread::scope(|scope| {
        if has_companions {
            scope.spawn(|| {
                while !stop_watch.load(Ordering::Relaxed) {
                    if ctx.interrupted.load(Ordering::Relaxed) {
                        step_interrupted.store(true, Ordering::Relaxed);
                        break;
                    }
                    let dead = {
                        let mut list = companions.processes.lock().expect("companion lock");
                        list.iter_mut()
                            .any(|child| matches!(child.try_wait(), Ok(Some(_))))
                    };
                    if dead {
                        companion_exited.store(true, Ordering::Relaxed);
                        step_interrupted.store(true, Ordering::Relaxed);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            });
        }
        let rctx = oms_command_runs::Context::new(
            &cwd,
            if has_companions {
                &step_interrupted
            } else {
                ctx.interrupted
            },
        );
        let res = match rctx {
            Ok(ref rctx) => {
                let mut sink = std::io::sink();
                let mut on_spawn = |pid: u32| {
                    spawned_pid = Some(pid);
                    if let Some(attempts) = obj_get_mut(run, "steps")
                        .and_then(|steps| obj_get_mut(steps, step_id))
                        .and_then(|rec| obj_get_mut(rec, "attempts"))
                        .and_then(|arr| arr.array_mut().ok())
                        && let Some(last) = attempts.last_mut()
                    {
                        let _ = last.set("pid", number(pid));
                        if let Err(err) = catalog::save(ctx, run_path, run) {
                            pid_persist_err = Some(err);
                        }
                    }
                };
                oms_command_runs::process::execute_with_hooks(
                    rctx,
                    &argv,
                    &policy,
                    false,
                    max_output,
                    Some(Duration::from_secs(timeout_secs)),
                    &mut collector,
                    &mut sink,
                    Some(&mut on_spawn),
                )
            }
            Err(e) => Err(e),
        };
        stop_watch.store(true, Ordering::Relaxed);
        res
    });
    if let Some(err) = pid_persist_err {
        return Err(err);
    }

    let elapsed = started.elapsed().as_secs_f64();
    let companion_died = companion_exited.load(Ordering::Relaxed);
    let was_interrupted = ctx.interrupted.load(Ordering::Relaxed) && !companion_died;

    let (code, reason) = if was_interrupted {
        (130, "interrupted")
    } else if companion_died {
        (125, "companion-exited")
    } else {
        match exec_result {
            Ok(out) => {
                if spawned_pid.is_none() {
                    (127, "launch-failed")
                } else if out.code == 124 {
                    (124, "timeout")
                } else if out.code == 130 && !ctx.interrupted.load(Ordering::Relaxed) {
                    (out.code, "exit")
                } else {
                    (out.code, "exit")
                }
            }
            Err(err) => {
                if err.message.contains("exceeds its byte limit") {
                    (125, "output-limit")
                } else if err.code == 130 {
                    return Ok(StepOutcome {
                        code: 130,
                        reason: "interrupted",
                        duration_seconds: elapsed,
                        console_bytes: collector.bytes,
                        console_sha256: collector.digest_hex(),
                        sensitive: collector.sensitive,
                        interrupted: true,
                    });
                } else {
                    (127, "launch-failed")
                }
            }
        }
    };

    Ok(StepOutcome {
        code,
        reason,
        duration_seconds: elapsed,
        console_bytes: collector.bytes,
        console_sha256: collector.digest_hex(),
        sensitive: collector.sensitive,
        interrupted: was_interrupted,
    })
}

fn mark_interrupted(run: &mut Value) -> Result<()> {
    run.set("status", string("interrupted"))?;
    let now = string(crate::timestamp(crate::epoch())?);
    if let Some(steps) = obj_get_mut(run, "steps")
        && let Some(items) = obj_items_mut(steps)
    {
        for (_, rec) in items {
            if rec.text("status") == "running" {
                rec.set("status", string("interrupted"))?;
                if let Some(attempts) = obj_get_mut(rec, "attempts")
                    && let Ok(arr) = attempts.array_mut()
                    && let Some(last) = arr.last_mut()
                    && last.text("status") == "running"
                {
                    last.set("status", string("interrupted"))?;
                    last.set("finished_at", now.clone())?;
                }
            }
        }
    }
    Ok(())
}

fn mark_failed(run: &mut Value, message: &str) -> Result<()> {
    run.set("status", string("failed"))?;
    run.set(
        "failure",
        object([("reason", string(sanitize_failure_reason(message)))]),
    )?;
    let now = string(crate::timestamp(crate::epoch())?);
    if let Some(steps) = obj_get_mut(run, "steps")
        && let Some(items) = obj_items_mut(steps)
    {
        for (_, rec) in items {
            if rec.text("status") == "running" {
                rec.set("status", string("failed"))?;
                if let Some(attempts) = obj_get_mut(rec, "attempts")
                    && let Ok(arr) = attempts.array_mut()
                    && let Some(last) = arr.last_mut()
                    && last.text("status") == "running"
                {
                    last.set("status", string("failed"))?;
                    last.set("finished_at", now.clone())?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn execute_plan(
    ctx: &Context<'_>,
    plan: &Value,
    opts: &Options,
    run_id: &str,
    resume: bool,
    request_id: Option<&str>,
    _services: &dyn oms_command_runs::Services,
) -> Result<(Value, i32)> {
    recipe_parent_only()?;
    require(
        opts.flag("execute"),
        "execution requires explicit --execute; use plan to inspect the invocation",
    )?;
    let run_path = record_path("runs", run_id)?;
    let _guard = storage::lock(&ctx.repo.join(&run_path))?;
    let mut run = if resume {
        let row = load(ctx, &run_path)?;
        require(
            row.get("schema").is_int(1) && row.text("id") == run_id,
            "run is missing or invalid",
        )?;
        require(
            row.get("plan_digest") == plan.get("plan_digest"),
            "definition, inputs, environment or declared dependencies changed; start a new run",
        )?;
        verify_completed(&ctx.repo, &row, plan)?;
        if row.text("status") == "succeeded" {
            for old in row.get("outputs").array().unwrap_or(&[]) {
                let req = object([
                    ("path", old.get("path").clone()),
                    ("max_bytes", number(64 * 1024 * 1024)),
                ]);
                let actual = output_entry(&ctx.repo, &req)?;
                let same = actual.as_ref().is_some_and(|a| {
                    a.text("sha256") == old.text("sha256") && a.get("bytes") == old.get("bytes")
                });
                require(
                    same,
                    format!(
                        "completed final output changed; start a new run: {}",
                        old.text("path")
                    ),
                )?;
            }
            return Ok((row, 0));
        }
        let mut uncertain = matches!(row.text("status"), "running" | "interrupted");
        for (_, rec) in row.get("steps").object()? {
            if matches!(rec.text("status"), "running" | "interrupted") {
                uncertain = true;
                if let Some(last) = rec.get("attempts").array()?.last() {
                    require(
                        !pid_alive(last.get("pid")),
                        "a prior step process still appears live; resume cannot replay it",
                    )?;
                }
            }
        }
        for comp in row.get("companion_attempts").array().unwrap_or(&[]) {
            if comp.text("status") == "running" {
                require(
                    !pid_alive(comp.get("pid")),
                    "a prior companion still appears live; resume cannot duplicate it",
                )?;
            }
        }
        require(
            !uncertain || opts.flag("retry-uncertain"),
            "interrupted execution has uncertain side effects; --retry-uncertain is required",
        )?;
        require(
            row.text("status") != "failed" || opts.flag("retry-failed"),
            "failed execution requires explicit --retry-failed",
        )?;
        row
    } else {
        require(!ctx.repo.join(&run_path).exists(), "run id already exists")?;
        require(
            records(ctx, "runs")?.len() < MAX_RECORDS,
            "recipe run capacity reached",
        )?;
        let mut steps_map = json::empty();
        for step in plan.get("steps").array()? {
            steps_map.set(
                step.text("id"),
                object([
                    ("status", string("pending")),
                    ("attempts", Value::Array(vec![])),
                ]),
            )?;
        }
        let companion_ids =
            json::list(plan.get("companions").array()?.iter().map(|c| c.text("id")));
        object([
            ("schema", number(1)),
            ("id", string(run_id)),
            ("recipe_id", plan.get("id").clone()),
            ("kind", plan.get("kind").clone()),
            ("action", plan.get("action").clone()),
            ("source", plan.get("source").clone()),
            ("definition_digest", plan.get("definition_digest").clone()),
            ("overlay_digest", plan.get("overlay_digest").clone()),
            ("plan_digest", plan.get("plan_digest").clone()),
            ("inputs", plan.get("inputs").clone()),
            ("ports", plan.get("ports").clone()),
            ("companions", companion_ids),
            ("created_at", string(crate::timestamp(crate::epoch())?)),
            ("request_id", request_id.map(string).unwrap_or(Value::Null)),
            (
                "outputs_before",
                snapshot_outputs(&ctx.repo, plan.get("outputs"))?,
            ),
            ("steps", steps_map),
            ("companion_attempts", Value::Array(vec![])),
            (
                "execution",
                object([
                    ("profile", string("trusted-local")),
                    ("sandbox", Value::Bool(false)),
                    ("network", string("host")),
                    (
                        concat!("credential", "s"),
                        string("ambient host files remain readable"),
                    ),
                    ("process_groups", Value::Bool(true)),
                ]),
            ),
        ])
    };

    run.set("status", string("running"))?;
    run.set("updated_at", string(crate::timestamp(crate::epoch())?))?;
    catalog::save(ctx, &run_path, &run)?;
    catalog::history_with(
        ctx,
        if resume { "run-resumed" } else { "run-started" },
        run_id,
        &[
            ("recipe_id", plan.get("id").clone()),
            ("plan_digest", plan.get("plan_digest").clone()),
        ],
    )?;

    let companions = CompanionGuard::new();
    let pending_steps: Vec<Value> = plan
        .get("steps")
        .array()?
        .iter()
        .filter(|s| run.get("steps").get(s.text("id")).text("status") != "succeeded")
        .cloned()
        .collect();

    let mut exit_code: i32 = 0;
    let body: Result<()> = (|| {
        if !pending_steps.is_empty() {
            for comp in plan.get("companions").array()? {
                let comp_count = run.get("companion_attempts").array()?.len();
                require(comp_count < MAX_ATTEMPTS, "companion attempt limit reached")?;
                let comp_cmd = comp.get("command");
                if let Ok(port_num) = comp.get("ready_port").int() {
                    let port =
                        u16::try_from(port_num).map_err(|_| error(2, "invalid companion port"))?;
                    TcpListener::bind(("127.0.0.1", port))
                        .map_err(|_| error(2, "companion port is already in use"))?;
                }
                let comp_row = object([
                    ("id", comp.get("id").clone()),
                    ("status", string("running")),
                    ("started_at", string(crate::timestamp(crate::epoch())?)),
                    ("command_digest", comp_cmd.get("command_digest").clone()),
                ]);
                obj_get_mut(&mut run, "companion_attempts")
                    .and_then(|v| v.array_mut().ok())
                    .ok_or_else(|| error(2, "invalid companion attempts"))?
                    .push(comp_row);
                catalog::save(ctx, &run_path, &run)?;

                verify_command_inputs(&ctx.repo, comp_cmd)?;
                let cwd = confined(&ctx.repo, comp_cmd.text("cwd"))?;
                let argv = command_argv(comp_cmd)?;
                let env = command_env(comp_cmd)?;
                let child = Command::new(&argv[0])
                    .args(&argv[1..])
                    .current_dir(&cwd)
                    .env_clear()
                    .envs(env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .map_err(|_| error(2, "local process or filesystem operation failed"))?;
                let child_pid = child.id();
                if let Some(arr) =
                    obj_get_mut(&mut run, "companion_attempts").and_then(|v| v.array_mut().ok())
                    && let Some(last) = arr.last_mut()
                {
                    last.set("pid", number(child_pid))?;
                }
                catalog::save(ctx, &run_path, &run)?;
                {
                    let mut list = companions.processes.lock().expect("companion lock");
                    list.push(child);
                }

                if let Ok(port_num) = comp.get("ready_port").int() {
                    let port =
                        u16::try_from(port_num).map_err(|_| error(2, "invalid companion port"))?;
                    let startup_secs = u64::try_from(comp.get("startup_seconds").int()?)
                        .map_err(|_| error(2, "invalid companion startup timeout"))?;
                    let deadline = Instant::now() + Duration::from_secs(startup_secs);
                    let addr = SocketAddr::from(([127, 0, 0, 1], port));
                    loop {
                        if ctx.interrupted.load(Ordering::Relaxed) {
                            return Err(error(130, "interrupted"));
                        }
                        {
                            let mut list = companions.processes.lock().expect("companion lock");
                            if let Some(last) = list.last_mut()
                                && matches!(last.try_wait(), Ok(Some(_)))
                            {
                                return Err(error(2, "companion exited before readiness"));
                            }
                        }
                        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
                            break;
                        }
                        if Instant::now() >= deadline {
                            return Err(error(2, "companion readiness timed out"));
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }

        let has_companions =
            !plan.get("companions").array()?.is_empty() && !pending_steps.is_empty();
        for step in &pending_steps {
            let step_id = step.text("id");
            let attempt_num = {
                let rec = run.get("steps").get(step_id);
                let attempts = rec.get("attempts").array()?;
                require(attempts.len() < MAX_ATTEMPTS, "step attempt limit reached")?;
                attempts.len() + 1
            };
            let before = snapshot_outputs(&ctx.repo, step.get("outputs"))?;
            let step_cmd = step.get("command");
            let current = object([
                ("attempt", number(attempt_num)),
                ("status", string("running")),
                ("started_at", string(crate::timestamp(crate::epoch())?)),
                ("command_digest", step_cmd.get("command_digest").clone()),
                (
                    "environment_digest",
                    step_cmd.get("environment_digest").clone(),
                ),
                (
                    "executable_digest",
                    step_cmd.get("executable_digest").clone(),
                ),
                (
                    "executable_path_digest",
                    step_cmd.get("executable_path_digest").clone(),
                ),
                ("dependencies", step_cmd.get("dependencies").clone()),
            ]);
            let planned_bytes = json::pretty(&run)?.len() + json::pretty(&current)?.len();
            require(
                planned_bytes <= storage::MAX_FILE - 1024 * 1024,
                "run receipt capacity reached; start a new run",
            )?;
            {
                let rec = obj_get_mut(&mut run, "steps")
                    .and_then(|s| obj_get_mut(s, step_id))
                    .ok_or_else(|| error(2, "missing step state"))?;
                rec.set("status", string("running"))?;
                obj_get_mut(rec, "attempts")
                    .and_then(|a| a.array_mut().ok())
                    .ok_or_else(|| error(2, "invalid step attempts"))?
                    .push(current);
            }
            catalog::save(ctx, &run_path, &run)?;

            let outcome = run_step_command(
                ctx,
                step_cmd,
                &companions,
                has_companions,
                &run_path,
                &mut run,
                step_id,
            )?;
            if outcome.interrupted {
                mark_interrupted(&mut run)?;
                exit_code = 130;
                return Ok(());
            }
            let finished_at = string(crate::timestamp(crate::epoch())?);
            let dur_str = format!("{:.3}", outcome.duration_seconds);
            {
                let rec = obj_get_mut(&mut run, "steps")
                    .and_then(|s| obj_get_mut(s, step_id))
                    .ok_or_else(|| error(2, "missing step state"))?;
                if let Some(last) = obj_get_mut(rec, "attempts")
                    .and_then(|a| a.array_mut().ok())
                    .and_then(|arr| arr.last_mut())
                {
                    last.set("exit", number(outcome.code))?;
                    last.set("reason", string(outcome.reason))?;
                    last.set("duration_seconds", Value::Number(dur_str))?;
                    last.set("console_bytes", number(outcome.console_bytes))?;
                    last.set("console_sha256", string(outcome.console_sha256))?;
                    last.set("sensitive_console_detected", Value::Bool(outcome.sensitive))?;
                    last.set("console_retained", Value::Bool(false))?;
                    last.set("finished_at", finished_at)?;
                }
                if outcome.code != 0 {
                    rec.set("status", string("failed"))?;
                    if let Some(last) = obj_get_mut(rec, "attempts")
                        .and_then(|a| a.array_mut().ok())
                        .and_then(|arr| arr.last_mut())
                    {
                        last.set("status", string("failed"))?;
                    }
                    exit_code = outcome.code;
                }
            }
            if outcome.code != 0 {
                run.set(
                    "failure",
                    object([
                        ("step", string(step_id)),
                        ("reason", string(outcome.reason)),
                    ]),
                )?;
                run.set("status", string("failed"))?;
                return Ok(());
            }
            let outputs = verify_outputs(&ctx.repo, step.get("outputs"), &before)?;
            {
                let rec = obj_get_mut(&mut run, "steps")
                    .and_then(|s| obj_get_mut(s, step_id))
                    .ok_or_else(|| error(2, "missing step state"))?;
                rec.set("status", string("succeeded"))?;
                if let Some(last) = obj_get_mut(rec, "attempts")
                    .and_then(|a| a.array_mut().ok())
                    .and_then(|arr| arr.last_mut())
                {
                    last.set("outputs", outputs)?;
                    last.set("status", string("succeeded"))?;
                }
            }
            catalog::save(ctx, &run_path, &run)?;
            catalog::history_with(
                ctx,
                "step-succeeded",
                run_id,
                &[("step", string(step_id)), ("attempt", number(attempt_num))],
            )?;
        }

        verify_completed(&ctx.repo, &run, plan)?;
        let final_outputs =
            verify_outputs(&ctx.repo, plan.get("outputs"), run.get("outputs_before"))?;
        run.set("outputs", final_outputs)?;
        run.remove("failure");
        run.set("status", string("succeeded"))?;
        Ok(())
    })();

    companions.stop_all();
    let stopped_at = string(crate::timestamp(crate::epoch())?);
    if let Some(arr) = obj_get_mut(&mut run, "companion_attempts").and_then(|v| v.array_mut().ok())
    {
        for row in arr.iter_mut() {
            row.set("status", string("stopped"))?;
            row.set("finished_at", stopped_at.clone())?;
        }
    }
    if let Err(err) = body {
        if err.code == 130 || ctx.interrupted.load(Ordering::Relaxed) {
            mark_interrupted(&mut run)?;
            exit_code = 130;
        } else {
            mark_failed(&mut run, &err.message)?;
            exit_code = 2;
        }
    } else if exit_code != 0 && run.text("status") == "running" {
        run.set("status", string("failed"))?;
    }
    run.set("updated_at", string(crate::timestamp(crate::epoch())?))?;
    catalog::save(ctx, &run_path, &run)?;
    catalog::history_with(
        ctx,
        "run-finished",
        run_id,
        &[
            ("status", run.get("status").clone()),
            ("exit", number(exit_code)),
        ],
    )?;
    Ok((run, exit_code))
}

fn execute(
    ctx: &Context<'_>,
    plan: &Value,
    opts: &Options,
    resume_id: Option<&str>,
    services: &dyn oms_command_runs::Services,
) -> Result<Output> {
    let run_id = match resume_id {
        Some(id) => id.to_owned(),
        None => crate::random_id("run-")?,
    };
    let (run, exit_code) = execute_plan(
        ctx,
        plan,
        opts,
        &run_id,
        resume_id.is_some(),
        None,
        services,
    )?;
    json_output(exit_code, &run)
}

pub(crate) fn run(
    args: &[OsString],
    ctx: &Context<'_>,
    services: &dyn oms_command_runs::Services,
) -> Result<Output> {
    let opts = Options::parse(
        args,
        &[
            "repo", "input", "overlay", "port", "with", "file", "id", "limit", "digest",
            "resource", "now", "event-id", "data",
        ],
        &[
            "json",
            "execute",
            "retry-failed",
            "retry-uncertain",
            "replace",
        ],
    )?;
    if opts.flag("help") {
        return Ok(crate::success(HELP));
    }
    let repo = opts.repo(ctx)?;
    let ctx = Context {
        runtime: ctx.runtime,
        cwd: repo.clone(),
        repo: repo.clone(),
        state_repo: repo,
        interrupted: ctx.interrupted,
    };
    let action = opts
        .pos
        .first()
        .map(String::as_str)
        .ok_or_else(|| error(2, HELP))?;
    match action {
        "validate" | "plan" | "run" | "resume" | "workspace" => {
            let workspace = action == "workspace";
            let resume = action == "resume";
            let (operation, file, selection) = if workspace {
                require(
                    opts.pos.len() == 4,
                    "workspace requires plan|run FILE ACTION",
                )?;
                (
                    opts.pos[1].as_str(),
                    opts.pos[2].as_str(),
                    opts.pos[3].as_str(),
                )
            } else if resume {
                require(
                    opts.pos.len() == 2 && !opts.get("file").is_empty(),
                    "resume requires ID --file FILE",
                )?;
                ("run", opts.get("file"), "")
            } else {
                require(opts.pos.len() == 2, "action requires FILE")?;
                (action, opts.pos[1].as_str(), "")
            };
            require(["validate", "plan", "run"].contains(&operation), HELP)?;
            if operation == "run" {
                recipe_parent_only()?;
                require(
                    opts.flag("execute"),
                    "execution requires explicit --execute; use plan to inspect the invocation",
                )?;
            }
            let loaded = catalog::source(&ctx, file)?;
            if operation == "validate" {
                let (definition, digest, _) = bundle(&ctx, &loaded, &opts)?;
                return json_output(
                    0,
                    &object([
                        ("schema", number(1)),
                        ("valid", Value::Bool(true)),
                        ("id", definition.get("id").clone()),
                        ("kind", definition.get("kind").clone()),
                        ("definition_digest", string(digest)),
                    ]),
                );
            }
            let plan = if resume {
                let previous = load(&ctx, &record_path("runs", &opts.pos[1])?)?;
                require(
                    previous.get("schema").is_int(1) && previous.text("id") == opts.pos[1],
                    "run is missing or invalid",
                )?;
                let companions = previous
                    .get("companions")
                    .strings()?
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                compile_with(
                    &ctx,
                    loaded,
                    opts.get("overlay"),
                    previous.get("inputs"),
                    previous.get("ports"),
                    &companions,
                    previous.text("action"),
                )?
            } else {
                require(
                    (loaded.definition.text("kind") == "workspace") == workspace,
                    "workspace definitions use workspace plan/run; recipes use plan/run",
                )?;
                compile(&ctx, loaded, &opts, selection)?
            };
            if operation == "run" {
                execute(
                    &ctx,
                    &plan,
                    &opts,
                    resume.then(|| opts.pos[1].as_str()),
                    services,
                )
            } else {
                json_output(0, &public_plan(&plan)?)
            }
        }
        "show" => {
            require(opts.pos.len() == 2, "show requires ID")?;
            json_output(0, &load(&ctx, &record_path("runs", &opts.pos[1])?)?)
        }
        "history" => {
            require(opts.pos.len() == 1, "history takes options only")?;
            let (mut rows, bad) = storage::rows(&ctx.repo, &Path::new(BASE).join("history.jsonl"))?;
            require(bad == 0, "malformed recipe history")?;
            if !opts.get("id").is_empty() {
                rows.retain(|r| r.text("id") == opts.get("id"));
            }
            let limit = opts.number("limit", 100)?;
            let rows = rows.split_off(rows.len().saturating_sub(limit));
            json_output(
                0,
                &object([("schema", number(1)), ("history", Value::Array(rows))]),
            )
        }
        "pack" | "workset" | "trigger" => catalog::run(&ctx, &opts, services),
        _ => Err(error(2, HELP)),
    }
}

fn expand_outputs(ctx: &Context<'_>, raw: &Value, bindings: &Value) -> Result<Value> {
    let rows = if matches!(raw, Value::Null) {
        &[][..]
    } else {
        raw.array()?
    };
    require(rows.len() <= 64, "too many output requirements")?;
    let mut names = BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        fields(
            row,
            &["path"],
            &["min_bytes", "max_bytes", "sha256", "fresh"],
        )?;
        let path = expand(row.get("path").str()?, bindings)?;
        confined(&ctx.repo, &path)?;
        require(names.insert(path.clone()), "duplicate output path")?;
        let low = bounded(row.get("min_bytes"), 0, 0, 64 * 1024 * 1024)?;
        let high = bounded(row.get("max_bytes"), 16 * 1024 * 1024, 1, 64 * 1024 * 1024)?;
        require(low <= high, "output min_bytes exceeds max_bytes")?;
        if let Some(d) = row.opt("sha256") {
            json::digest(d)?;
        }
        if let Some(f) = row.opt("fresh") {
            require(matches!(f, Value::Bool(_)), "output fresh must be boolean")?;
        }
        let mut item = row.clone();
        item.set("path", string(path))?;
        result.push(item);
    }
    Ok(Value::Array(result))
}
fn plan_signature(plan: &Value) -> Result<Value> {
    let mut result = json::empty();
    for key in [
        "id",
        "kind",
        "action",
        "definition_digest",
        "overlay_digest",
        "effective_digest",
        "source",
        "inputs",
        "ports",
        "outputs",
    ] {
        result.set(key, plan.get(key).clone())?;
    }
    for key in ["steps", "companions"] {
        let mut rows = Vec::new();
        for step in plan.get(key).array()? {
            let mut row = step.clone();
            let mut command = json::empty();
            for field in [
                "name",
                "command_digest",
                "cwd",
                "environment_digest",
                "dependencies",
                "executable_digest",
                "executable_path_digest",
                "timeout_seconds",
                "max_output_bytes",
            ] {
                command.set(field, step.get("command").get(field).clone())?;
            }
            row.set("command", command)?;
            rows.push(row);
        }
        result.set(key, Value::Array(rows))?;
    }
    Ok(result)
}

fn validate_outputs(raw: &Value) -> Result<()> {
    let rows = if matches!(raw, Value::Null) {
        &[][..]
    } else {
        raw.array()?
    };
    require(rows.len() <= 64, "too many output requirements")?;
    let mut paths = BTreeSet::new();
    for row in rows {
        fields(
            row,
            &["path"],
            &["min_bytes", "max_bytes", "sha256", "fresh"],
        )?;
        let path = row.get("path").str()?;
        let re = regex::Regex::new(r"\$\{(input|port|resource)\.[A-Za-z][A-Za-z0-9_-]{0,63}\}")
            .expect("static expression");
        let literal = re.replace_all(path, "bound");
        require(!literal.contains("${"), "unsupported output interpolation")?;
        let relative = helpers::relative(&literal)?;
        require(
            !relative.components().any(|c| c.as_os_str() == ".oms") && paths.insert(path),
            "duplicate or private output path",
        )?;
        let low = bounded(row.get("min_bytes"), 0, 0, 64 * 1024 * 1024)?;
        let high = bounded(row.get("max_bytes"), 16 * 1024 * 1024, 1, 64 * 1024 * 1024)?;
        require(low <= high, "output min_bytes exceeds max_bytes")?;
        if let Some(value) = row.opt("sha256") {
            json::digest(value)?;
        }
        if let Some(value) = row.opt("fresh") {
            require(
                matches!(value, Value::Bool(_)),
                "output fresh must be boolean",
            )?;
        }
    }
    Ok(())
}

fn template(raw: &str) -> Result<String> {
    let expression = regex::Regex::new(r"\$\{(input|port|resource)\.[A-Za-z][A-Za-z0-9_-]{0,63}\}")
        .expect("static expression");
    let literal = expression.replace_all(raw, "bound").into_owned();
    require(
        !literal.contains("${") && raw.len() <= 4096 && !raw.chars().any(char::is_control),
        "invalid or unsupported interpolation",
    )?;
    Ok(literal)
}
fn data_template(raw: &str, dot: bool) -> Result<()> {
    let value = template(raw)?;
    if dot && value == "." {
        return Ok(());
    }
    let relative = helpers::relative(&value)?;
    require(
        !relative.components().any(|c| c.as_os_str() == ".oms"),
        "recipe paths cannot target OMS state",
    )
}
