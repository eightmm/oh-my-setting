use super::*;
use std::collections::{BTreeMap, BTreeSet};
pub const TYPES: &[&str] = &[
    "run_started",
    "node_started",
    "node_outcome",
    "gate_decision",
    "route_evaluated",
    "run_finished",
    "note",
];
pub fn directory(repo: &Path, run: &str) -> Result<PathBuf> {
    if !regex::Regex::new(r"^run-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{8}$")
        .unwrap()
        .is_match(run)
    {
        return Err(Error::new(format!("invalid run id: {run}")));
    }
    Ok(repo.join(".oms/graph/runs").join(run))
}
fn lock(repo: &Path, run: &str) -> Result<oms_lock::Guard> {
    let path = directory(repo, run)?.join("events.jsonl");
    let mut settings = oms_lock::Settings::from_env().map_err(|e| Error::new(e.to_string()))?;
    settings.cwd = repo.into();
    let lock = oms_lock::Lock::for_file(&path, settings).map_err(|e| Error::new(e.to_string()))?;
    lock.try_acquire().map_err(|e| Error::new(e.to_string()))
}
pub fn load(repo: &Path, run: &str) -> Result<Value> {
    let path = directory(repo, run)?.join("graph.json");
    let raw = super::read(&path)
        .map_err(|_| Error::new(format!("graph run has no valid frozen spec: {run}")))?;
    normalize(&raw)
}
pub fn read(repo: &Path, run: &str) -> Result<Vec<Value>> {
    let path = directory(repo, run)?.join("events.jsonl");
    let store = oms_command_plan::storage::Store::open(repo)?;
    let raw = store
        .optional(&path, 256 * 1024 * 1024)?
        .unwrap_or_default();
    let mut rows = vec![];
    for line in raw
        .split(|b| *b == b'\n')
        .filter(|b| !b.iter().all(u8::is_ascii_whitespace))
    {
        let value = Value::parse(line)
            .map_err(|_| Error::new(format!("invalid event row in graph run {run}")))?;
        if value.get("schema") != &Value::from(1_i64)
            || st(&value, "run_id") != run
            || !TYPES.contains(&st(&value, "event"))
        {
            return Err(Error::new(format!("invalid event row in graph run {run}")));
        }
        rows.push(value);
    }
    Ok(rows)
}
pub fn start(repo: &Path, spec: &Value, run: &str, options: Value) -> Result<String> {
    let graph = normalize(spec)?;
    let verdict = validate(&graph);
    if !verdict.get("ok").truth() {
        return Err(Error::new(format!(
            "cannot start an invalid graph: {}",
            ar(&verdict, "errors")
                .iter()
                .map(|r| st(r, "code"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let run = if run.is_empty() {
        let stamp = util::now().replace(['-', ':'], "");
        format!("run-{stamp}-{}", oms_command_plan::storage::random_hex(4)?)
    } else {
        run.into()
    };
    let _guard = lock(repo, &run)?;
    let dir = directory(repo, &run)?;
    for file in ["graph.json", "events.jsonl"] {
        if std::fs::symlink_metadata(dir.join(file)).is_ok() {
            return Err(Error::new(format!("graph run already exists: {run}")));
        }
    }
    util::create_marker(repo)?;
    write(&dir.join("graph.json"), &graph)?;
    let fields = obj([
        ("idempotency_key", "run:start".into()),
        ("spec_id", graph.get("id").clone()),
        ("spec_digest", digest(&graph)?.into()),
        ("options", options),
    ]);
    append_unlocked(repo, &run, "run_started", fields)?;
    Ok(run)
}
pub fn append(repo: &Path, run: &str, event: &str, fields: Value) -> Result<Value> {
    let _guard = lock(repo, run)?;
    append_unlocked(repo, run, event, fields)
}
fn append_unlocked(repo: &Path, run: &str, event: &str, fields: Value) -> Result<Value> {
    if !TYPES.contains(&event) {
        return Err(Error::new(format!("unknown graph event: {event}")));
    }
    load(repo, run).map_err(|_| Error::new(format!("graph run does not exist: {run}")))?;
    let rows = read(repo, run)?;
    let key = st(&fields, "idempotency_key");
    if !key.is_empty() {
        if key.len() > 200
            || !regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$")
                .unwrap()
                .is_match(key)
        {
            return Err(Error::new("invalid idempotency key"));
        }
        if rows.iter().any(|r| st(r, "idempotency_key") == key) {
            return Err(Error::new(format!(
                "duplicate graph event idempotency key: {key}"
            )));
        }
    }
    let detail = st(&fields, "detail");
    if !detail.is_empty() && oms_graph_python::sensitive_text(detail) {
        return Err(Error::new("graph event detail contains sensitive text"));
    }
    let reserved: Vec<&str> = ["schema", "seq", "ts", "run_id", "event"]
        .into_iter()
        .filter(|key| fields.has(key))
        .collect();
    if !reserved.is_empty() {
        return Err(Error::new(format!(
            "event fields cannot replace: {}",
            reserved.join(", ")
        )));
    }
    let last = rows
        .last()
        .map(|r| r.get("seq").clone())
        .unwrap_or(0_i64.into());
    if !last.integer(false) || number_cmp(&last, &0_i64.into()).is_lt() {
        return Err(Error::new("graph event log has an invalid final sequence"));
    }
    let seq = oms_command_operations::value::add(&last.py(), "1");
    let mut row = obj([
        ("schema", 1_i64.into()),
        ("seq", Value::Num(seq)),
        ("ts", util::now().into()),
        ("run_id", run.into()),
        ("event", event.into()),
    ]);
    for (key, value) in map(&fields) {
        row.set(&key, value);
    }
    if fields.has("detail") {
        row.set(
            "detail",
            detail
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(500)
                .collect::<String>(),
        );
    }
    let store = oms_command_plan::storage::Store::open(repo)?;
    let data = row.json(false).map_err(Error::new)?;
    let row = oms_command_plan::json::parse(&data)?;
    store.append(directory(repo, run)?.join("events.jsonl"), &row)?;
    Value::parse(&data).map_err(Error::new)
}
pub fn project(rows: &[Value], spec: &Value) -> Result<Value> {
    let graph = normalize(spec)?;
    let mut state = route::state(&graph, &empty(), &empty(), &empty(), &empty())?;
    let mut nodes = map(state.get("nodes"));
    let mut gates = empty();
    let mut repeats = empty();
    let mut bindings = empty();
    let mut seen = BTreeSet::new();
    let mut attempts = BTreeSet::new();
    let mut children: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for raw in rows {
        if !matches!(raw, Value::Obj(_)) {
            continue;
        }
        let key = st(raw, "idempotency_key");
        let identity = if key.is_empty() {
            format!("row:{}", util::sha(raw.json(false).map_err(Error::new)?))
        } else {
            format!("key:{key}")
        };
        if !seen.insert(identity) {
            continue;
        }
        let event = st(raw, "event");
        let raw_node = st(raw, "node");
        let (parent, child) = raw_node.split_once('.').unwrap_or((raw_node, ""));
        if !child.is_empty() && st(graph.get("nodes").get(parent), "kind") == "subgraph" {
            let mut row = raw.clone();
            row.set("node", child);
            children.entry(parent.into()).or_default().push(row);
            if ["node_started", "node_outcome", "gate_decision"].contains(&event) {
                attempts.insert((raw_node.to_owned(), raw.default("attempt", 1_i64).py()));
            }
            continue;
        }
        let Some(node) = nodes.get_mut(raw_node).filter(|_| TYPES.contains(&event)) else {
            if event == "route_evaluated" {
                for (key, value) in map(raw
                    .default("route", raw.clone())
                    .get("budget")
                    .get("repeats"))
                {
                    if value.integer(false)
                        && !number_cmp(&value, &repeats.default(&key, 0_i64)).is_lt()
                    {
                        repeats.set(&key, value);
                    }
                }
            }
            continue;
        };
        let mut attempt = raw.get("attempt").clone();
        if !attempt.integer(true) {
            attempt = maximum(node.get("attempts"), &1_i64.into());
            if event == "node_started" && st(node, "status") != "active" {
                attempt = increment(node.get("attempts"));
            }
        }
        if ["node_started", "node_outcome", "gate_decision"].contains(&event)
            && matches!(raw.get("seq"), Value::Num(n) if !n.contains(['.','e','E']))
        {
            node.set("seq", maximum(&node.default("seq", 0_i64), raw.get("seq")));
        }
        let task = st(raw, "task_id");
        match event {
            "node_started" => {
                node.set("attempts", maximum(node.get("attempts"), &attempt));
                node.set("status", "active");
                node.set("outcome", Value::Null);
                node.set("claimed_outcome", Value::Null);
                if !task.is_empty() {
                    node.set("task_id", task);
                }
                attempts.insert((raw_node.into(), attempt.py()));
            }
            "node_outcome" => {
                node.set("attempts", maximum(node.get("attempts"), &attempt));
                node.set("status", "finished");
                node.set(
                    "claimed_outcome",
                    raw.default("claimed_outcome", raw.get("outcome").clone()),
                );
                node.set(
                    "outcome",
                    raw.default("outcome", raw.get("claimed_outcome").clone()),
                );
                if !task.is_empty() {
                    node.set("task_id", task);
                }
                attempts.insert((raw_node.into(), attempt.py()));
            }
            "gate_decision" => {
                let decision = raw.default("outcome", raw.default("decision", "")).py();
                if !decision.is_empty() {
                    gates.set(raw_node, decision.as_str());
                    node.set("attempts", maximum(node.get("attempts"), &attempt));
                    node.set("status", "finished");
                    node.set("claimed_outcome", decision.as_str());
                    node.set("outcome", decision);
                    attempts.insert((raw_node.into(), attempt.py()));
                }
            }
            _ => {}
        }
        if ["node_started", "node_outcome"].contains(&event)
            && !task.is_empty()
            && !st(raw, "binding").is_empty()
        {
            bindings.set(
                st(raw, "binding"),
                obj([
                    ("task_id", task.into()),
                    ("node", raw_node.into()),
                    ("attempt", attempt),
                ]),
            );
        }
    }
    let mut child_states = BTreeMap::new();
    for (parent, rows) in children {
        let child = graph
            .get("subgraphs")
            .get(st(graph.get("nodes").get(&parent), "graph"));
        if matches!(child, Value::Obj(_)) {
            child_states.insert(parent, project(&rows, child)?);
        }
    }
    state.set("nodes", object(nodes));
    state.set("steps", attempts.len());
    state.set("repeats", repeats);
    state.set("gates", gates);
    state.set("bindings", bindings);
    state.set("subgraphs", object(child_states));
    Ok(state)
}
pub fn latest(repo: &Path) -> Result<String> {
    let root = repo.join(".oms/graph/runs");
    if !root.is_dir() || root.is_symlink() {
        return Ok(String::new());
    }
    let mut names = vec![];
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir()
            && directory(repo, &name).is_ok()
            && load(repo, &name).is_ok()
            && read(repo, &name).is_ok()
        {
            names.push(name);
        }
    }
    names.sort();
    Ok(names.pop().unwrap_or_default())
}
