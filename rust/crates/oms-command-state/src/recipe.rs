//! Explicit local recipes. Durable running receipts make interrupted effects uncertain.
use crate::json::{number, object, string};
use crate::{
    Context, Options, Output, Result, Value, error, helpers, json, json_output, require, storage,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const HELP: &str = "Usage: oms recipe [--repo PATH] validate|plan|run FILE [--input NAME=VALUE]... [--overlay FILE] [--execute]\n       oms recipe resume ID --file FILE [--execute] [--retry-failed] [--retry-uncertain]\n       oms recipe show ID\n       oms recipe history [--id ID] [--limit N]\n       oms recipe workspace plan|run FILE ACTION [--input NAME=VALUE]... [--port NAME=N]... [--execute]\n       oms recipe pack inspect|import DIRECTORY [--digest SHA] [--resource NAME]...\n       oms recipe pack list\n       oms recipe workset put FILE [--replace]\n       oms recipe workset show ID\n       oms recipe workset list\n       oms recipe trigger add FILE\n       oms recipe trigger list|requests|tick|event|enable|disable|plan|cancel|run ...\nHost execution requires --execute. Membership and stored requests grant no authority.\n";
const BASE: &str = ".oms/recipes";
#[path = "recipe_catalog.rs"]
mod catalog;
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
    load_bounded(ctx, path, 256 * 1024)
}
fn load_bounded(ctx: &Context<'_>, path: &Path, limit: usize) -> Result<Value> {
    let raw = storage::read(&ctx.repo, path, limit, false)?;
    let value = json::parse(&raw)?;
    value.object()?;
    helpers::scrub(&value)?;
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
        if supplied_value.is_some() && ["integer", "number", "boolean"].contains(&spec.text("type"))
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

fn bundle(ctx: &Context<'_>, loaded: &Loaded, opts: &Options) -> Result<(Value, String, Value)> {
    let mut definition = loaded.definition.clone();
    validate(&definition)?;
    let original = json::hash(&definition)?;
    let mut overlay_digest = Value::Null;
    if !opts.get("overlay").is_empty() {
        require(
            definition.text("kind") == "recipe",
            "project overlays currently apply to recipes",
        )?;
        let overlay = catalog::source(ctx, opts.get("overlay"))?.definition;
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

fn compile(ctx: &Context<'_>, loaded: Loaded, opts: &Options, action: &str) -> Result<Value> {
    let (definition, original, overlay_digest) = bundle(ctx, &loaded, opts)?;
    let input = inputs(definition.get("inputs"), &pairs(opts.all("input"))?)?;
    let mut ports = definition.opt("ports").cloned().unwrap_or_else(json::empty);
    for (name, value) in pairs(opts.all("port"))?.object()? {
        let name = std::str::from_utf8(name).map_err(|_| error(2, "invalid port name"))?;
        require(ports.opt(name).is_some(), "unknown workspace port")?;
        ports.set(
            name,
            number(
                value
                    .str()?
                    .parse::<u16>()
                    .map_err(|_| error(2, "invalid port number"))?,
            ),
        )?;
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
    require(opts.all("with").len() <= 8, "too many workspace companions")?;
    for name in opts.all("with") {
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
                string(
                    cwd.strip_prefix(&ctx.repo)
                        .unwrap_or(&cwd)
                        .to_string_lossy(),
                ),
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

fn execute(
    _ctx: &Context<'_>,
    _plan: &Value,
    opts: &Options,
    _resume: Option<&str>,
    _services: &dyn oms_command_runs::Services,
) -> Result<Output> {
    crate::parent_only()?;
    require(opts.flag("execute"), "host execution requires --execute")?;
    Err(error(
        2,
        "native recipe execution requires a child-start PID receipt API; no process or state write was started",
    ))
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
            let plan = compile(&ctx, loaded, &opts, selection)?;
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
