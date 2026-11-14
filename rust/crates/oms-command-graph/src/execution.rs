//! Lossless GraphSpec normalization, validation, pure routing and event replay.
#[path = "execution_commit.rs"]
mod commit;
#[path = "execution_events.rs"]
pub mod events;
#[path = "execution_render.rs"]
mod render;
#[path = "execution_route.rs"]
pub mod route;
#[path = "execution_runner.rs"]
mod runner;
#[path = "execution_scheduler.rs"]
pub mod scheduler;
#[path = "execution_validate.rs"]
mod validate;
#[path = "execution_workspace.rs"]
pub mod workspace;

use crate::{CommandResult, Error, Result, cli::Args, util};
pub use oms_command_operations::value::{V as Value, obj};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
pub use validate::{validate, validate_with_plan_tasks};
pub const OUTCOMES: &[&str] = &[
    "completed",
    "failed",
    "unverified",
    "partial",
    "blocked",
    "changes_requested",
    "approved",
    "skipped",
];
pub fn st<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).text().unwrap_or("")
}
pub fn ar<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).array()
}
pub fn empty() -> Value {
    obj([])
}
pub fn list(items: impl IntoIterator<Item = impl Into<Value>>) -> Value {
    Value::Arr(items.into_iter().map(Into::into).collect())
}
pub fn map(value: &Value) -> BTreeMap<String, Value> {
    if let Value::Obj(fields) = value {
        fields
            .iter()
            .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v.clone()))
            .collect()
    } else {
        BTreeMap::new()
    }
}
pub fn object(items: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Obj(
        items
            .into_iter()
            .map(|(k, v)| (k.into_bytes(), v))
            .collect(),
    )
}
pub fn integer(value: &Value) -> i64 {
    if let Value::Num(n) = value {
        n.parse().unwrap_or(if n.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        })
    } else {
        0
    }
}
pub fn count(value: &Value, key: &str) -> i64 {
    integer(value.get(key))
}
pub fn number_cmp(left: &Value, right: &Value) -> std::cmp::Ordering {
    let a = left.py();
    let b = right.py();
    let an = a.starts_with('-');
    let bn = b.starts_with('-');
    match (an, bn) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => {
            let order = oms_command_task::packet::decimal_cmp(
                a.trim_start_matches('-').as_bytes(),
                b.trim_start_matches('-').as_bytes(),
            );
            if an { order.reverse() } else { order }
        }
    }
}
pub fn maximum(left: &Value, right: &Value) -> Value {
    if number_cmp(left, right).is_lt() {
        right.clone()
    } else {
        left.clone()
    }
}
pub fn increment(value: &Value) -> Value {
    Value::Num(oms_command_operations::value::add(&value.py(), "1"))
}
fn source_bytes(path: &Path) -> Result<Vec<u8>> {
    util::no_symlinks(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("file has no parent"))?;
    let store = oms_command_plan::storage::Store::open(parent)?;
    Ok(store.read(
        Path::new(
            path.file_name()
                .ok_or_else(|| Error::new("file has no name"))?,
        ),
        256 * 1024 * 1024,
    )?)
}
fn read(path: &Path) -> Result<Value> {
    Value::parse(&source_bytes(path)?).map_err(Error::new)
}
pub fn write(path: &Path, value: &Value) -> Result<()> {
    let mut raw = value.json(false).map_err(Error::new)?;
    raw.push(b'\n');
    util::write(path, &raw)
}
pub fn install_root() -> Option<PathBuf> {
    std::env::var_os("OMS_INSTALL_ROOT")
        .map(PathBuf::from)
        .filter(|p| {
            p.is_absolute() && (p.join("config/graphs").is_dir() || p.join("scripts/oms").is_file())
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().and_then(Path::parent).map(Path::to_path_buf))
                .filter(|p| p.join("config/graphs").is_dir() || p.join("scripts/oms").is_file())
        })
}
pub fn load(source: &Value, cwd: &Path) -> Result<Value> {
    load_with_root(source, cwd, None)
}
pub fn load_with_root(source: &Value, cwd: &Path, root: Option<&Path>) -> Result<Value> {
    let raw = if matches!(source, Value::Obj(_)) {
        source.clone()
    } else if let Some(text) = source.text() {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::new("graph spec source is empty"));
        }
        let path = util::at(cwd, text);
        if path.is_file() {
            read(&path)?
        } else if let Some(path) = root
            .map(Path::to_path_buf)
            .or_else(install_root)
            .map(|r| r.join("config/graphs").join(format!("{text}.json")))
            .filter(|p| p.is_file())
        {
            read(&path)?
        } else {
            Value::parse(text.as_bytes()).map_err(|_| {
                Error::new("graph spec is not a path, bundled name, or JSON object: invalid JSON")
            })?
        }
    } else {
        return Err(Error::new(
            "graph spec source must be a path, name, JSON text, or mapping",
        ));
    };
    if !matches!(raw, Value::Obj(_)) {
        return Err(Error::new("graph spec must be a JSON object"));
    }
    normalize(&raw)
}
fn strings(value: &Value, label: &str) -> Result<Value> {
    if matches!(value, Value::Null) {
        return Ok(list(Vec::<Value>::new()));
    }
    if !matches!(value, Value::Arr(_)) || value.array().iter().any(|v| !matches!(v, Value::Str(_)))
    {
        return Err(Error::new(format!("{label} must be a list of strings")));
    }
    Ok(value.clone())
}
pub fn normalize(raw: &Value) -> Result<Value> {
    let (root, children) = normalize_shallow(raw, st(raw, "id"), None)?;
    let mut stack = vec![(String::new(), root, children.into_iter())];
    loop {
        let frame = stack.last_mut().unwrap();
        if let Some((name, child)) = frame.2.next() {
            if !matches!(child, Value::Obj(_)) {
                return Err(Error::new(format!("subgraph {name} must be an object")));
            }
            let (node, children) = normalize_shallow(child, &name, Some(frame.1.get("schema")))?;
            stack.push((name, node, children.into_iter()));
        } else {
            let (name, node, _) = stack.pop().unwrap();
            if let Some(parent) = stack.last_mut() {
                let mut children = parent.1.get("subgraphs").clone();
                children.set(&name, node);
                parent.1.set("subgraphs", children);
            } else {
                return Ok(node);
            }
        }
    }
}
fn normalize_shallow<'a>(
    raw: &'a Value,
    id: &str,
    inherited: Option<&Value>,
) -> Result<(Value, Vec<(String, &'a Value)>)> {
    if !matches!(raw, Value::Obj(_)) {
        return Err(Error::new("graph spec must be an object"));
    }
    let raw_nodes = raw.default("nodes", empty());
    if !matches!(raw_nodes, Value::Obj(_)) {
        return Err(Error::new("nodes must be an object"));
    }
    let raw_edges = raw.default("edges", list(Vec::<Value>::new()));
    if !matches!(raw_edges, Value::Arr(_)) {
        return Err(Error::new("edges must be a list"));
    }
    let mut nodes = BTreeMap::new();
    for (id, mut node) in map(&raw_nodes) {
        if !matches!(node, Value::Obj(_)) {
            return Err(Error::new(format!("node {id} must be an object")));
        }
        let kind = st(&node, "kind").to_owned();
        node.set("title", node.default("title", id.as_str()).py());
        if kind != "terminal" {
            node.set(
                "requires",
                strings(node.get("requires"), &format!("node {id} requires"))?,
            );
        }
        if matches!(kind.as_str(), "agent" | "tool") {
            node.set("effect", node.default("effect", "read"));
            node.set(
                "proof",
                strings(node.get("proof"), &format!("node {id} proof"))?,
            );
        }
        if kind == "agent" {
            if node.get("plan_task").truth() || node.get("plan_task_from").truth() {
                node.set("mode", node.default("mode", "run"));
            }
            for field in ["bind_task", "plan_task_from", "context"] {
                if matches!(node.get(field), Value::Null) {
                    if let Value::Obj(m) = &mut node {
                        m.remove(field.as_bytes());
                    }
                }
            }
            if matches!(node.get("context"), Value::Obj(_)) {
                node.set(
                    "context",
                    obj([
                        ("task", node.get("context").default("task", "")),
                        (
                            "max_files",
                            node.get("context").default("max_files", 12_i64),
                        ),
                    ]),
                );
            }
        }
        if kind == "tool" {
            node.set("timeout", node.default("timeout", 600_i64));
            node.set("cacheable", node.default("cacheable", false));
            if matches!(node.get("tool"), Value::Null) {
                if let Value::Obj(fields) = &mut node {
                    fields.remove(b"tool".as_slice());
                }
            }
            if !raw_nodes.get(&id).has("effect") {
                if let Some(effect) = validate::capability_effect(st(&node, "tool")) {
                    node.set("effect", effect);
                }
            }
        }
        if kind == "gate" {
            node.set("authority", node.default("authority", "parent"));
            node.set(
                "decisions",
                strings(
                    &node.default("decisions", list(["approved", "changes_requested"])),
                    &format!("node {id} decisions"),
                )?,
            );
        }
        node.set("join", node.default("join", "all"));
        nodes.insert(id, node);
    }
    let mut edges = vec![];
    for (i, raw_edge) in raw_edges.array().iter().enumerate() {
        if !matches!(raw_edge, Value::Obj(_)) {
            return Err(Error::new(format!("edge {i} must be an object")));
        }
        let mut edge = raw_edge.clone();
        for field in ["from", "to"] {
            edge.set(field, edge.default(field, "").py());
        }
        for field in ["outcomes", "when"] {
            edge.set(
                field,
                strings(edge.get(field), &format!("edge {i} {field}"))?,
            );
        }
        edge.set("kind", edge.default("kind", "normal"));
        edge.set("priority", edge.default("priority", 0_i64));
        edge.set("fanout", edge.default("fanout", false));
        edges.push(edge);
    }
    let budget = raw.default("budget", empty());
    if !matches!(budget, Value::Obj(_)) {
        return Err(Error::new("budget must be an object"));
    }
    let Value::Obj(fields) = raw else {
        unreachable!()
    };
    let mut result = Value::Obj(
        fields
            .iter()
            .filter(|(key, _)| key.as_slice() != b"subgraphs")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    result.set(
        "schema",
        raw.default("schema", inherited.cloned().unwrap_or(1_i64.into())),
    );
    result.set(
        "id",
        raw.default("id", if id.is_empty() { "graph" } else { id })
            .py(),
    );
    result.set("entry", raw.default("entry", "").py());
    result.set(
        "budget",
        obj([
            ("max_steps", budget.default("max_steps", 20_i64)),
            ("max_repeats", budget.default("max_repeats", 3_i64)),
        ]),
    );
    result.set("stop_facts", strings(raw.get("stop_facts"), "stop_facts")?);
    result.set("nodes", object(nodes));
    result.set("edges", list(edges));
    if raw.has("subgraphs") && !matches!(raw.get("subgraphs"), Value::Obj(_)) {
        return Err(Error::new("subgraphs must be an object"));
    }
    let children = if let Value::Obj(fields) = raw.get("subgraphs") {
        fields
            .iter()
            .map(|(name, child)| (String::from_utf8_lossy(name).into_owned(), child))
            .collect()
    } else {
        vec![]
    };
    if raw.has("subgraphs") {
        result.set("subgraphs", empty());
    }
    Ok((result, children))
}

pub fn digest(spec: &Value) -> Result<String> {
    Ok(util::sha(normalize(spec)?.json(false).map_err(Error::new)?))
}
pub fn predicate(text: &str) -> Result<(String, &'static str, String)> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err(Error::new("empty fact predicate"));
    }
    let (key, op, value) = if let Some((k, v)) = raw.split_once("!=") {
        (k, "ne", v)
    } else if let Some((k, v)) = raw.split_once('=') {
        (k, "eq", v)
    } else if let Some(k) = raw.strip_prefix('!') {
        (k, "falsy", "")
    } else {
        (raw, "truthy", "")
    };
    let key = key.trim();
    if !regex::Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_.:-]{0,199}$")
        .unwrap()
        .is_match(key)
    {
        return Err(Error::new(format!("invalid fact key in predicate: {raw}")));
    }
    Ok((key.into(), op, value.trim().into()))
}
pub fn holds(text: &str, facts: &Value) -> bool {
    let Ok((key, op, expected)) = predicate(text) else {
        return false;
    };
    let present = facts.has(&key);
    let value = facts.get(&key);
    let rendered = match value {
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Null => String::new(),
        _ => value.py(),
    };
    match op {
        "truthy" => present && value.truth(),
        "falsy" => !present || !value.truth(),
        "eq" => present && rendered == expected,
        _ => !present || rendered != expected,
    }
}
pub fn missing(predicates: &[Value], facts: &Value) -> Vec<Value> {
    predicates
        .iter()
        .filter(|v| !holds(v.text().unwrap_or(""), facts))
        .cloned()
        .collect()
}
pub fn effective(node: &Value, claimed: &str, facts: &Value) -> (String, Vec<Value>) {
    if !matches!(claimed, "completed" | "approved") {
        return (claimed.into(), vec![]);
    }
    let absent = missing(ar(node, "proof"), facts);
    (
        if absent.is_empty() {
            claimed.into()
        } else {
            "unverified".into()
        },
        absent,
    )
}
pub fn bound(state: &Value, name: &str) -> String {
    st(state.get("bindings").get(name), "task_id").into()
}
pub fn binding_facts(state: &Value, facts: &Value) -> Value {
    let mut result = facts.clone();
    for (name, entry) in map(state.get("bindings")) {
        let task = st(&entry, "task_id");
        if task.is_empty() {
            continue;
        }
        result.set(&format!("binding.{name}.task_id"), task);
        for (key, value) in map(facts) {
            if let Some(rest) = key.strip_prefix(&format!("plan.task.{task}.")) {
                result.set(&format!("binding.{name}.{rest}"), value);
            } else {
                let parts: Vec<_> = key.split('.').collect();
                if parts.len() >= 4 && parts[0] == "receipt" && parts[2] == task {
                    result.set(
                        &format!(
                            "binding.{name}.receipt.{}.{}",
                            parts[1],
                            parts[3..].join(".")
                        ),
                        value,
                    );
                }
            }
        }
    }
    result
}
pub(crate) fn dispatch(
    args: &Args,
    repo: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
    helper: Option<&Path>,
    _runtime_services: Option<&dyn oms_runtime::Services>,
    services: &(dyn oms_command_plan::Services + Sync),
    host: &dyn crate::HostServices,
) -> Result<CommandResult> {
    let mut text = String::new();
    let mut code = 0;
    let err = String::new();
    let value = match args.action.as_str() {
        "validate" => {
            let spec = load_with_root(&args.text("spec").into(), cwd, host.install_root())?;
            let verdict = validate(&spec);
            if !verdict.get("ok").truth() {
                code = 3;
            } else {
                text.push_str("ok\n");
            }
            for label in ["errors", "warnings"] {
                for row in ar(&verdict, label) {
                    text.push_str(&format!(
                        "{} {} {}: {}\n",
                        if label == "errors" {
                            "error"
                        } else {
                            "warning"
                        },
                        st(row, "code"),
                        st(row, "where"),
                        st(row, "message")
                    ));
                }
            }
            verdict
        }
        "render" => {
            let spec = load_with_root(&args.text("spec").into(), cwd, host.install_root())?;
            text = render::exec(&spec, None, None, args.flag("mermaid"));
            spec
        }
        "route" => {
            let (spec, state, facts) = if !args.text("run").is_empty() {
                runner::view(repo, args.text("run"), cancel, services)?
            } else if !args.text("spec").is_empty() {
                let spec = load_with_root(&args.text("spec").into(), cwd, host.install_root())?;
                let outcomes = option(args.text("outcomes"), cwd)?;
                let state = route::state(&spec, &outcomes, &empty(), &empty(), &empty())?;
                let facts = if args.text("facts").is_empty() {
                    runner::facts(repo, cancel, services)?
                } else {
                    read(&util::at(cwd, args.text("facts")))?
                };
                if !matches!(facts, Value::Obj(_)) {
                    return Err(Error::new(format!(
                        "fact file must hold a JSON object: {}",
                        args.text("facts")
                    )));
                }
                (spec, state, facts)
            } else {
                return Err(Error::new("exec route needs a spec or --run"));
            };
            let result = route::evaluate(&spec, &state, &facts);
            text = render::route_text(&result);
            result
        }
        "events" => {
            let rows = events::read(repo, args.text("run"))?;
            let selected = if args.int("limit") > 0 {
                &rows[rows.len().saturating_sub(args.int("limit") as usize)..]
            } else {
                &rows
            };
            for row in selected {
                text.push_str(&format!(
                    "{} {} {} {} {}\n",
                    row.get("seq").py(),
                    st(row, "ts"),
                    st(row, "event"),
                    if st(row, "node").is_empty() {
                        "-"
                    } else {
                        st(row, "node")
                    },
                    row.get("outcome").or(row.get("status").or("-")).py()
                ));
            }
            obj([
                ("schema", 1_i64.into()),
                ("run_id", args.text("run").into()),
                ("events", list(selected.iter().cloned())),
            ])
        }
        "status" => {
            let run = if args.text("run").is_empty() {
                events::latest(repo)?
            } else {
                args.text("run").into()
            };
            if run.is_empty() {
                return Ok((3,vec![],b"graph: no execution graph run is recorded; run: oms graph exec run SPEC --worker P\n".to_vec()));
            }
            if args.flag("json") && args.flag("mermaid") {
                return Err(Error::new(
                    "exec status cannot combine --json with --mermaid",
                ));
            }
            let (spec, state, facts) = runner::view(repo, &run, cancel, services)?;
            let route = route::evaluate(&spec, &state, &facts);
            text = if args.flag("mermaid") {
                render::exec(&spec, Some(&state), None, true)
            } else {
                format!(
                    "run: {run} spec={} status={}\n{}",
                    st(&spec, "id"),
                    st(&route, "status"),
                    render::exec(&spec, Some(&state), Some(&route), false)
                )
            };
            obj([
                ("schema", 1_i64.into()),
                ("run_id", run.into()),
                ("spec_id", spec.get("id").clone()),
                ("route", route),
                ("projection", state),
            ])
        }
        "test" => {
            let path = util::at(cwd, args.text("path"));
            let mut files = if path.is_dir() {
                std::fs::read_dir(&path)?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect::<Vec<_>>()
            } else {
                vec![path]
            };
            files.sort();
            if files.is_empty() {
                return Err(Error::new(format!(
                    "no route fixtures found: {}",
                    args.text("path")
                )));
            }
            let mut rows = vec![];
            for path in files {
                let raw = source_bytes(&path)?;
                let fixture = Value::parse(&raw).map_err(Error::new)?;
                let source = oms_context::parse_experiment_json(&raw, 256 * 1024 * 1024)
                    .map_err(Error::new)?;
                let keys = if let oms_context::Json::Object(fields) = &source {
                    fields
                        .iter()
                        .find(|(key, _)| key.as_slice() == b"expect")
                        .and_then(|(_, value)| {
                            if let oms_context::Json::Object(fields) = value {
                                Some(
                                    fields
                                        .iter()
                                        .map(|(key, _)| key.clone())
                                        .collect::<Vec<_>>(),
                                )
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };
                if !matches!(fixture, Value::Obj(_)) {
                    return Err(Error::new(format!(
                        "route fixture must be a JSON object: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )));
                }
                let row = route::fixture_with_order(&fixture, cwd, keys.as_deref())?;
                let name = Value::from(
                    fixture
                        .get("name")
                        .or(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
                        .py(),
                );
                let ok = row.get("diff").array().is_empty();
                if !ok {
                    code = 3;
                }
                text.push_str(&format!(
                    "{} {}{}\n",
                    if ok { "PASS" } else { "FAIL" },
                    name.py(),
                    if ok {
                        String::new()
                    } else {
                        format!(
                            ": {}",
                            String::from_utf8_lossy(
                                &row.get("diff").json(false).map_err(Error::new)?
                            )
                        )
                    }
                ));
                rows.push(obj([
                    ("name", name),
                    ("ok", ok.into()),
                    ("diff", row.get("diff").clone()),
                ]));
            }
            obj([
                ("schema", 1_i64.into()),
                ("ok", (code == 0).into()),
                ("results", list(rows)),
            ])
        }
        "run" | "resume" => {
            let result = runner::run_with_host(args, repo, cwd, cancel, helper, services, host)?;
            if result.get("dry_run").truth() {
                text = format!(
                    "run: dry-run status={} primary={}\n",
                    st(result.get("route"), "status"),
                    result.get("route").get("primary").or("-").py()
                );
                for command in ar(&result, "commands") {
                    text.push_str(&format!(
                        "command: {}\n",
                        command
                            .array()
                            .iter()
                            .map(Value::py)
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
            } else {
                text = format!(
                    "run: {} status={} primary={}\n",
                    st(&result, "run_id"),
                    st(&result, "status"),
                    result.get("primary").or("-").py()
                );
                if !st(&result, "reason").is_empty() {
                    text.push_str(&format!("reason: {}\n", st(&result, "reason")));
                }
                if ["gate", "blocked", "exhausted", "waiting", "invalid"]
                    .contains(&st(&result, "status"))
                {
                    code = 3;
                }
            }
            result
        }
        "decide" => {
            let result = runner::decide(
                repo,
                args.text("run"),
                args.text("node"),
                args.text("outcome"),
                args.text("note"),
                cancel,
                services,
            )?;
            text = format!(
                "decide: {} {}={}\n{}",
                args.text("run"),
                args.text("node"),
                args.text("outcome"),
                render::route_text(result.get("route"))
            );
            result
        }
        "commit" => {
            let result = commit::bound_with_host(
                repo,
                args.text("binding"),
                args.text("run"),
                args.text("message"),
                cancel,
                services,
                host,
            )?;
            text = format!(
                "commit: {} task={} status={} paths={}\n",
                st(&result, "commit").chars().take(12).collect::<String>(),
                st(&result, "task_id"),
                st(&result, "status"),
                ar(&result, "paths").len()
            );
            result
        }
        _ => return Err(Error::new("unknown execution action")),
    };
    let mut value = value;
    if !args.text("html_fragment").is_empty() {
        let path = Path::new(args.text("html_fragment"));
        if !path.is_absolute() {
            return Err(Error::new(
                "--html-fragment requires an absolute output path",
            ));
        }
        let fragment = if args.action == "render" {
            render::html(&value, None, None)
        } else {
            let run = if args.text("run").is_empty() {
                events::latest(repo)?
            } else {
                args.text("run").to_owned()
            };
            let spec = events::load(repo, &run)?;
            render::html(
                &spec,
                Some(value.get("projection")),
                Some(value.get("route")),
            )
        };
        if fragment.len() > 1024 * 1024 {
            return Err(Error::new(
                "HTML fragment exceeds the 1 MiB agent-view limit",
            ));
        }
        util::write(path, fragment.as_bytes())?;
        value.set("html_fragment", path.to_string_lossy().into_owned());
        text = format!("html_fragment: {}\n", path.display());
    }
    let out = if args.flag("json") {
        let mut raw = value.json(args.flag("pretty")).map_err(Error::new)?;
        if args.flag("pretty") && raw.last() == Some(&b'\n') {
            raw.pop();
        }
        raw.push(b'\n');
        raw
    } else {
        text.into_bytes()
    };
    Ok((code, out, err.into_bytes()))
}
fn option(raw: &str, cwd: &Path) -> Result<Value> {
    if raw.trim().is_empty() {
        return Ok(empty());
    }
    let value = if let Some(path) = raw.strip_prefix('@') {
        read(&util::at(cwd, path))?
    } else {
        Value::parse(raw.as_bytes())
            .map_err(|_| Error::new("option is not a JSON object: invalid JSON"))?
    };
    if !matches!(value, Value::Obj(_)) {
        return Err(Error::new("option must be a JSON object"));
    }
    Ok(value)
}
