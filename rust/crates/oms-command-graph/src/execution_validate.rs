use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub fn capability_effect(name: &str) -> Option<&'static str> {
    match name {
        "plan_acceptance" | "project_context" => Some("read"),
        "commit_bound" => Some("write"),
        _ => None,
    }
}
pub fn valid_binding(value: &Value) -> bool {
    value.text().is_some_and(|s| {
        regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_-]{0,63}$")
            .unwrap()
            .is_match(s)
    })
}
fn row(code: &str, where_: &str, message: impl Into<Value>) -> Value {
    obj([
        ("code", code.into()),
        ("where", where_.into()),
        ("message", message.into()),
    ])
}
fn add(rows: &mut Vec<Value>, code: &str, where_: &str, message: impl Into<Value>) {
    let candidate = row(code, where_, message);
    if !rows.contains(&candidate) {
        rows.push(candidate);
    }
}
fn predicates(value: &Value, where_: &str, errors: &mut Vec<Value>) {
    if !matches!(value, Value::Arr(_)) || value.array().iter().any(|v| !matches!(v, Value::Str(_)))
    {
        add(
            errors,
            "invalid_fact_reference",
            where_,
            "fact predicates must be a list of strings",
        );
        return;
    }
    for item in value.array() {
        if let Err(error) = predicate(item.text().unwrap_or("")) {
            add(
                errors,
                "invalid_fact_reference",
                where_,
                format!("{}: {}", item.py(), error.message),
            );
        }
    }
}
fn reach(start: &str, adj: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut pending = adj.get(start).cloned().unwrap_or_default();
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if seen.insert(id.clone()) {
            pending.extend(adj.get(&id).into_iter().flatten().cloned());
        }
    }
    seen
}
#[allow(clippy::too_many_arguments)]
fn graph<'a>(
    raw: &'a Value,
    prefix: &str,
    inherited: &Value,
    nested: bool,
    errors: &mut Vec<Value>,
    warnings: &mut Vec<Value>,
    children: &mut Vec<(&'a Value, String, Value)>,
    plan_tasks: Option<&[String]>,
) {
    if !matches!(raw, Value::Obj(_)) {
        add(errors, "invalid_schema", prefix, "graph must be an object");
        return;
    }
    let schema = raw.default("schema", inherited.clone());
    if matches!(schema, Value::Bool(_)) || schema != 1_i64.into() {
        add(
            errors,
            "invalid_schema",
            &format!("{prefix}.schema"),
            "schema must be 1",
        );
    }
    let mut nodes = map(raw.get("nodes"));
    if let Value::Arr(items) = raw.get("nodes") {
        for (i, node) in items.iter().enumerate() {
            let where_ = format!("{prefix}.nodes[{i}]");
            if !matches!(node, Value::Obj(_)) {
                add(errors, "invalid_kind", &where_, "node must be an object");
                continue;
            }
            let id = st(node, "id");
            if nodes.contains_key(id) {
                add(
                    errors,
                    "duplicate_node",
                    &where_,
                    format!("duplicate node id {id}"),
                );
            } else {
                nodes.insert(id.into(), node.clone());
            }
        }
    } else if raw.has("nodes") && !matches!(raw.get("nodes"), Value::Obj(_)) {
        add(
            errors,
            "invalid_kind",
            &format!("{prefix}.nodes"),
            "nodes must be an object",
        );
    }
    let edges = ar(raw, "edges");
    if raw.has("edges") && !matches!(raw.get("edges"), Value::Arr(_)) {
        add(
            errors,
            "unknown_endpoint",
            &format!("{prefix}.edges"),
            "edges must be a list",
        );
    }
    let entry = st(raw, "entry");
    if !raw
        .get("entry")
        .text()
        .is_some_and(|id| nodes.contains_key(id))
    {
        add(
            errors,
            "missing_entry",
            &format!("{prefix}.entry"),
            "entry must name an existing node",
        );
    }
    let budget = raw.default("budget", empty());
    if !matches!(budget, Value::Obj(_)) {
        add(
            errors,
            "invalid_budget",
            &format!("{prefix}.budget"),
            "budget must be an object",
        );
    }
    for (key, default, positive, message) in [
        (
            "max_steps",
            20_i64,
            true,
            "max_steps must be a positive integer",
        ),
        (
            "max_repeats",
            3_i64,
            false,
            "max_repeats must be a non-negative integer",
        ),
    ] {
        if !budget.default(key, default).integer(positive) {
            add(
                errors,
                "invalid_budget",
                &format!("{prefix}.budget.{key}"),
                message,
            );
        }
    }
    predicates(
        &raw.default("stop_facts", list(Vec::<Value>::new())),
        &format!("{prefix}.stop_facts"),
        errors,
    );
    let subgraphs = map(raw.get("subgraphs"));
    let mut terminals = 0;
    let mut writers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut readers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, node) in &nodes {
        let where_ = format!("{prefix}.nodes.{id}");
        let kind = st(node, "kind");
        if !["agent", "tool", "gate", "router", "subgraph", "terminal"].contains(&kind) {
            add(
                errors,
                "invalid_kind",
                &format!("{where_}.kind"),
                format!("unknown node kind {}", node.get("kind").py()),
            );
        }
        if kind == "terminal" {
            terminals += 1;
        }
        let effect = node.default("effect", "read");
        if ["agent", "tool"].contains(&kind)
            && !["read", "write", "none"].contains(&effect.text().unwrap_or(""))
        {
            add(
                errors,
                "invalid_effect",
                &format!("{where_}.effect"),
                format!("unknown effect {}", effect.py()),
            );
        }
        if !["all", "any"].contains(&node.default("join", "all").text().unwrap_or("")) {
            add(
                errors,
                "invalid_join",
                &format!("{where_}.join"),
                "join must be all or any",
            );
        }
        predicates(
            &node.default("requires", list(Vec::<Value>::new())),
            &format!("{where_}.requires"),
            errors,
        );
        if ["agent", "tool"].contains(&kind) {
            predicates(
                &node.default("proof", list(Vec::<Value>::new())),
                &format!("{where_}.proof"),
                errors,
            );
        }
        if kind == "agent" {
            let task = node.get("plan_task").text().is_some_and(|s| !s.is_empty());
            let source = !matches!(node.get("plan_task_from"), Value::Null);
            if task && source {
                add(
                    errors,
                    "invalid_task_binding",
                    &where_,
                    "plan_task and plan_task_from are mutually exclusive",
                );
            } else if !task && !source {
                add(
                    errors,
                    "invalid_plan_task_reference",
                    &format!("{where_}.plan_task"),
                    "agent node must name a plan task or a plan_task_from binding",
                );
            }
            if task
                && !source
                && st(node, "plan_task") != "next"
                && plan_tasks
                    .is_some_and(|tasks| !tasks.iter().any(|task| task == st(node, "plan_task")))
            {
                add(
                    errors,
                    "invalid_plan_task_reference",
                    &format!("{where_}.plan_task"),
                    format!("unknown plan task {}", st(node, "plan_task")),
                );
            }
            for field in ["plan_task_from", "bind_task"] {
                if !matches!(node.get(field), Value::Null) && !valid_binding(node.get(field)) {
                    add(
                        errors,
                        "invalid_task_binding",
                        &format!("{where_}.{field}"),
                        format!("{field} must be a binding identifier"),
                    );
                }
            }
            if !["run", "land"].contains(&node.default("mode", "run").text().unwrap_or("")) {
                add(
                    errors,
                    "invalid_plan_task_reference",
                    &format!("{where_}.mode"),
                    "agent mode must be run or land",
                );
            }
            for (field, mapping) in [
                ("bind_task", &mut writers),
                ("plan_task_from", &mut readers),
            ] {
                if !st(node, field).is_empty() {
                    mapping
                        .entry(st(node, field).into())
                        .or_default()
                        .push(id.clone());
                }
            }
            let context = node.get("context");
            if !matches!(context, Value::Null) {
                if !matches!(context, Value::Obj(_)) {
                    add(
                        errors,
                        "invalid_context",
                        &format!("{where_}.context"),
                        "context must be an object",
                    );
                } else {
                    if !matches!(context.default("task", ""), Value::Str(_)) {
                        add(
                            errors,
                            "invalid_context",
                            &format!("{where_}.context.task"),
                            "context.task must be a string",
                        );
                    }
                    let max = context.default("max_files", 12_i64);
                    if !max.integer(true) || integer(&max) > 200 {
                        add(
                            errors,
                            "invalid_context",
                            &format!("{where_}.context.max_files"),
                            "context.max_files must be an integer from 1 to 200",
                        );
                    }
                    let unknown: Vec<String> = map(context)
                        .into_keys()
                        .filter(|k| !matches!(k.as_str(), "task" | "max_files"))
                        .collect();
                    if !unknown.is_empty() {
                        add(
                            errors,
                            "invalid_context",
                            &format!("{where_}.context"),
                            format!("unknown context fields: {}", unknown.join(", ")),
                        );
                    }
                }
            }
        } else if !matches!(node.get("bind_task"), Value::Null) {
            add(
                errors,
                "invalid_task_binding",
                &format!("{where_}.bind_task"),
                "only an agent node may bind a task",
            );
        }
        if kind == "tool" {
            let command = !st(node, "command").trim().is_empty();
            let tool = st(node, "tool");
            if !tool.is_empty() {
                if let Some(expected) = capability_effect(tool) {
                    if command {
                        add(
                            errors,
                            "invalid_command",
                            &format!("{where_}.command"),
                            "a tool node names a capability or a command, not both",
                        );
                    }
                    if !matches!(node.get("effect"), Value::Null) && st(node, "effect") != expected
                    {
                        add(
                            errors,
                            "invalid_effect",
                            &format!("{where_}.effect"),
                            format!(
                                "capability {tool} has effect {expected}, not {}",
                                node.get("effect").py()
                            ),
                        );
                    }
                    if node.get("cacheable").truth() && tool != "project_context" {
                        add(
                            errors,
                            "invalid_command",
                            &format!("{where_}.cacheable"),
                            format!("capability {tool} is not cacheable"),
                        );
                    }
                    if tool == "project_context" {
                        if !matches!(node.get("task"), Value::Null | Value::Str(_)) {
                            add(
                                errors,
                                "invalid_command",
                                &format!("{where_}.task"),
                                "task must be a string",
                            );
                        }
                        if !matches!(node.get("max_files"), Value::Null)
                            && (!node.get("max_files").integer(true)
                                || count(node, "max_files") > 200)
                        {
                            add(
                                errors,
                                "invalid_command",
                                &format!("{where_}.max_files"),
                                "max_files must be an integer from 1 to 200",
                            );
                        }
                    }
                    if tool == "commit_bound" {
                        if matches!(node.get("binding"), Value::Null) {
                            add(
                                errors,
                                "invalid_command",
                                &format!("{where_}.binding"),
                                "capability commit_bound requires binding",
                            );
                        } else if !valid_binding(node.get("binding")) {
                            add(
                                errors,
                                "invalid_task_binding",
                                &format!("{where_}.binding"),
                                "binding must be a binding identifier",
                            );
                        }
                    }
                } else {
                    add(
                        errors,
                        "unknown_tool_capability",
                        &format!("{where_}.tool"),
                        format!(
                            "unknown tool capability {tool}; known: commit_bound, plan_acceptance, project_context"
                        ),
                    );
                }
            } else if !matches!(node.get("tool"), Value::Null) {
                add(
                    errors,
                    "unknown_tool_capability",
                    &format!("{where_}.tool"),
                    "tool must name a capability: commit_bound, plan_acceptance, project_context",
                );
            } else if !command {
                add(
                    errors,
                    "invalid_command",
                    &format!("{where_}.command"),
                    "tool command must be a non-empty string, or tool must name a capability",
                );
            } else if effect == "read".into() {
                add(
                    warnings,
                    "unverified_effect_declaration",
                    &format!("{where_}.command"),
                    "a command node's effect is declared, not verified; prefer a tool capability",
                );
            }
        }
        if kind == "gate" {
            let decisions = node.default("decisions", list(["approved", "changes_requested"]));
            if !["parent", "human"]
                .contains(&node.default("authority", "parent").text().unwrap_or(""))
                || !matches!(decisions, Value::Arr(_))
                || decisions.array().is_empty()
                || decisions
                    .array()
                    .iter()
                    .any(|v| !OUTCOMES.contains(&v.text().unwrap_or("")))
            {
                add(
                    errors,
                    "invalid_gate",
                    &where_,
                    "gate authority or decisions are invalid",
                );
            }
        }
        if kind == "subgraph" {
            if !node
                .get("graph")
                .text()
                .is_some_and(|name| subgraphs.contains_key(name))
            {
                add(
                    errors,
                    "unknown_subgraph",
                    &format!("{where_}.graph"),
                    "subgraph node names an unknown graph",
                );
            }
            if nested {
                add(
                    errors,
                    "recursive_subgraph",
                    &where_,
                    "subgraphs may only be one level deep",
                );
            }
        }
    }
    if terminals == 0 {
        add(
            errors,
            "missing_terminal",
            &format!("{prefix}.nodes"),
            "graph has no terminal node",
        );
    }
    let mut adjacency: BTreeMap<String, Vec<String>> =
        nodes.keys().map(|k| (k.clone(), vec![])).collect();
    let mut groups: BTreeMap<(String, String, String), Vec<&Value>> = BTreeMap::new();
    for (i, edge) in edges.iter().enumerate() {
        let where_ = format!("{prefix}.edges[{i}]");
        if !matches!(edge, Value::Obj(_)) {
            add(
                errors,
                "unknown_endpoint",
                &where_,
                "edge must be an object",
            );
            continue;
        }
        let source = st(edge, "from");
        let target = st(edge, "to");
        if !nodes.contains_key(source) || !nodes.contains_key(target) {
            add(
                errors,
                "unknown_endpoint",
                &where_,
                "edge endpoint does not exist",
            );
            continue;
        }
        adjacency.get_mut(source).unwrap().push(target.into());
        if st(&nodes[source], "kind") == "terminal" {
            add(
                errors,
                "terminal_outgoing_edge",
                &where_,
                "terminal nodes cannot have outgoing edges",
            );
        }
        let outcomes = ar(edge, "outcomes");
        let valid = matches!(edge.get("outcomes"), Value::Arr(_))
            && !outcomes.is_empty()
            && outcomes
                .iter()
                .all(|v| OUTCOMES.contains(&v.text().unwrap_or("")));
        if !valid {
            add(
                errors,
                "invalid_outcome",
                &format!("{where_}.outcomes"),
                "edge outcomes must be a non-empty list of known outcomes",
            );
        }
        if !["normal", "repeat"].contains(&edge.default("kind", "normal").text().unwrap_or("")) {
            add(
                errors,
                "invalid_repeat_edge",
                &format!("{where_}.kind"),
                "edge kind must be normal or repeat",
            );
        }
        let mut priority = edge.default("priority", 0_i64);
        if !matches!(&priority,Value::Num(n)if !n.contains(['.','e','E'])) {
            add(
                errors,
                "invalid_repeat_edge",
                &format!("{where_}.priority"),
                "edge priority must be an integer",
            );
            priority = 0_i64.into();
        }
        if !matches!(edge.default("fanout", false), Value::Bool(_)) {
            add(
                errors,
                "ambiguous_routes",
                &format!("{where_}.fanout"),
                "fanout must be boolean",
            );
        }
        predicates(
            &edge.default("when", list(Vec::<Value>::new())),
            &format!("{where_}.when"),
            errors,
        );
        if valid {
            for outcome in outcomes {
                groups
                    .entry((source.into(), outcome.py(), priority.py()))
                    .or_default()
                    .push(edge);
            }
        }
    }
    for ((source, outcome, priority), edges) in groups {
        if edges.len() > 1
            && !edges
                .iter()
                .all(|e| matches!(e.get("fanout"), Value::Bool(true)))
        {
            add(
                errors,
                "ambiguous_routes",
                &format!("{prefix}.edges"),
                format!(
                    "multiple routes from {source} for {outcome} at priority {priority} require fanout"
                ),
            );
        }
    }
    if nodes.contains_key(entry) {
        let mut reached = reach(entry, &adjacency);
        reached.insert(entry.into());
        for id in nodes.keys().filter(|id| !reached.contains(*id)) {
            add(
                errors,
                "unreachable_node",
                &format!("{prefix}.nodes.{id}"),
                "node is unreachable from entry",
            );
        }
    }
    for (name, ids) in &writers {
        if ids.len() > 1 {
            add(
                errors,
                "duplicate_task_binding_writer",
                &format!("{prefix}.nodes"),
                format!(
                    "binding {name} is written by more than one node: {}",
                    ids.join(", ")
                ),
            );
        }
    }
    for (id, node) in &nodes {
        if st(node, "kind") == "tool"
            && st(node, "tool") == "commit_bound"
            && valid_binding(node.get("binding"))
            && !writers.contains_key(st(node, "binding"))
        {
            add(
                errors,
                "unknown_task_binding",
                &format!("{prefix}.nodes.{id}.binding"),
                format!("no node binds {}", st(node, "binding")),
            );
        }
    }
    for (name, ids) in &readers {
        for id in ids {
            let where_ = format!("{prefix}.nodes.{id}.plan_task_from");
            if let Some(writers) = writers.get(name) {
                if !reach(&writers[0], &adjacency).contains(id) {
                    add(
                        errors,
                        "unreachable_task_binding",
                        &where_,
                        format!("{id} is not reachable from the node that binds {name}"),
                    );
                }
            } else {
                add(
                    errors,
                    "unknown_task_binding",
                    &where_,
                    format!("no node binds {name}"),
                );
            }
        }
    }
    if !raw.get("stop_facts").truth() {
        let reachability: BTreeMap<String, BTreeSet<String>> = nodes
            .keys()
            .map(|id| (id.clone(), reach(id, &adjacency)))
            .collect();
        let mut remaining: BTreeSet<String> = nodes.keys().cloned().collect();
        while let Some(seed) = remaining.first().cloned() {
            let component: BTreeSet<String> = remaining
                .iter()
                .filter(|id| {
                    **id == seed
                        || (reachability[&seed].contains(*id) && reachability[*id].contains(&seed))
                })
                .cloned()
                .collect();
            remaining.retain(|id| !component.contains(id));
            let cyclic = component.len() > 1 || adjacency[&seed].contains(&seed);
            let bounded = edges.iter().any(|e| {
                st(e, "kind") == "repeat"
                    && component.contains(st(e, "from"))
                    && component.contains(st(e, "to"))
            });
            if cyclic && !bounded {
                add(
                    errors,
                    "unbounded_cycle",
                    &format!("{prefix}.edges"),
                    "a cycle has no repeat edge or stop_facts policy",
                );
                break;
            }
        }
    }
    for (i, edge) in edges.iter().enumerate() {
        if st(edge, "kind") == "repeat"
            && nodes.contains_key(st(edge, "from"))
            && nodes.contains_key(st(edge, "to"))
        {
            let mut reached = reach(st(edge, "to"), &adjacency);
            reached.insert(st(edge, "to").into());
            if !reached.contains(st(edge, "from")) {
                add(
                    errors,
                    "invalid_repeat_edge",
                    &format!("{prefix}.edges[{i}]"),
                    "repeat edge must close a cycle",
                );
            }
        }
    }
    for (id, node) in &nodes {
        if st(node, "kind") == "terminal" {
            continue;
        }
        if adjacency[id].is_empty() {
            add(
                warnings,
                "dead_end",
                &format!("{prefix}.nodes.{id}"),
                "non-terminal node has no outgoing edge",
            );
        } else {
            let absent: Vec<&str> = ["failed", "unverified"]
                .into_iter()
                .filter(|outcome| {
                    !edges
                        .iter()
                        .filter(|e| st(e, "from") == id)
                        .any(|e| ar(e, "outcomes").iter().any(|v| v.text() == Some(outcome)))
                })
                .collect();
            if !absent.is_empty() {
                add(
                    warnings,
                    "unrouted_outcome",
                    &format!("{prefix}.nodes.{id}"),
                    format!("unrouted outcomes: {}", absent.join(", ")),
                );
            }
        }
    }
    if let Value::Obj(subgraphs) = raw.get("subgraphs") {
        for (name, child) in subgraphs {
            let name = String::from_utf8_lossy(name);
            let child_prefix = format!("{prefix}.subgraphs.{name}");
            if !matches!(child, Value::Obj(_)) {
                add(
                    errors,
                    "unknown_subgraph",
                    &child_prefix,
                    "subgraph must be an object",
                );
            } else {
                children.push((child, child_prefix, schema.clone()));
            }
        }
    }
}

pub fn validate(spec: &Value) -> Value {
    validate_with_plan_tasks(spec, None)
}
pub fn validate_with_plan_tasks(spec: &Value, plan_tasks: Option<&[String]>) -> Value {
    let mut errors = vec![];
    let mut warnings = vec![];
    let mut pending = vec![(spec, "spec".to_owned(), 1_i64.into())];
    while let Some((raw, prefix, schema)) = pending.pop() {
        graph(
            raw,
            &prefix,
            &schema,
            prefix != "spec",
            &mut errors,
            &mut warnings,
            &mut pending,
            plan_tasks,
        );
    }
    for rows in [&mut errors, &mut warnings] {
        rows.sort_by(|a, b| {
            (st(a, "where"), st(a, "code"), st(a, "message")).cmp(&(
                st(b, "where"),
                st(b, "code"),
                st(b, "message"),
            ))
        });
    }
    obj([
        ("ok", errors.is_empty().into()),
        ("errors", list(errors)),
        ("warnings", list(warnings)),
    ])
}
