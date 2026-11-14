use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub fn state(
    spec: &Value,
    outcomes: &Value,
    gates: &Value,
    repeats: &Value,
    bindings: &Value,
) -> Result<Value> {
    let spec = normalize(spec)?;
    let mut nodes: BTreeMap<String, Value> = map(spec.get("nodes"))
        .into_keys()
        .map(|id| {
            (
                id,
                obj([
                    ("status", "pending".into()),
                    ("outcome", Value::Null),
                    ("claimed_outcome", Value::Null),
                    ("attempts", 0_i64.into()),
                ]),
            )
        })
        .collect();
    let mut steps = 0_i64;
    let mut children: BTreeMap<String, Value> = BTreeMap::new();
    let mut child_gates: BTreeMap<String, Value> = BTreeMap::new();
    for (raw, outcome) in map(outcomes) {
        let (parent, child) = raw.split_once('.').unwrap_or((&raw, ""));
        if !child.is_empty() && st(spec.get("nodes").get(parent), "kind") == "subgraph" {
            children
                .entry(parent.into())
                .or_insert_with(empty)
                .set(child, outcome.py());
            steps += 1;
        } else if let Some(node) = nodes.get_mut(&raw) {
            node.set("status", "finished");
            node.set("outcome", outcome.py());
            node.set("claimed_outcome", outcome.py());
            node.set("attempts", 1_i64);
            steps += 1;
        }
    }
    for (raw, decision) in map(gates) {
        let (parent, child) = raw.split_once('.').unwrap_or((&raw, ""));
        if !child.is_empty() && st(spec.get("nodes").get(parent), "kind") == "subgraph" {
            child_gates
                .entry(parent.into())
                .or_insert_with(empty)
                .set(child, decision.py());
        } else if let Some(node) = nodes.get_mut(&raw)
            && st(node, "status") != "finished"
        {
            node.set("status", "finished");
            node.set("outcome", decision.py());
            node.set("claimed_outcome", decision.py());
            node.set("attempts", 1_i64);
            steps += 1;
        }
    }
    let mut child_states = BTreeMap::new();
    let names: BTreeSet<String> = children.keys().chain(child_gates.keys()).cloned().collect();
    for parent in names {
        let name = st(spec.get("nodes").get(&parent), "graph");
        let child = spec.get("subgraphs").get(name);
        if matches!(child, Value::Obj(_)) {
            child_states.insert(
                parent.clone(),
                state(
                    child,
                    children.get(&parent).unwrap_or(&empty()),
                    child_gates.get(&parent).unwrap_or(&empty()),
                    &empty(),
                    &empty(),
                )?,
            );
        }
    }
    let normalized_gates = object(
        map(gates)
            .into_iter()
            .map(|(name, value)| (name, value.py().into())),
    );
    let normalized_repeats = object(
        map(repeats)
            .into_iter()
            .map(|(name, value)| {
                oms_command_operations::value::integer_value(&value)
                    .map(|value| (name, value))
                    .map_err(Error::new)
            })
            .collect::<Result<Vec<_>>>()?,
    );
    let mut normalized_bindings = BTreeMap::new();
    for (name, value) in map(bindings) {
        let task = if matches!(value, Value::Obj(_)) {
            value.get("task_id").or("").py()
        } else {
            value.text().unwrap_or("").to_owned()
        };
        if !task.is_empty() {
            normalized_bindings.insert(
                name,
                obj([
                    ("task_id", task.into()),
                    ("node", value.get("node").or("").py().into()),
                    ("attempt", value.default("attempt", 1_i64)),
                ]),
            );
        }
    }
    Ok(obj([
        ("nodes", object(nodes)),
        ("steps", steps.into()),
        ("repeats", normalized_repeats),
        ("gates", normalized_gates),
        ("bindings", object(normalized_bindings)),
        ("subgraphs", object(child_states)),
    ]))
}
struct Eval {
    graph: Value,
    state: Value,
    facts: Value,
    nodes: BTreeMap<String, Value>,
    out: BTreeMap<String, Vec<Value>>,
    ins: BTreeMap<String, Vec<Value>>,
    reach: BTreeMap<String, BTreeSet<String>>,
    effective: BTreeMap<String, Value>,
    downgrades: Vec<Value>,
    trace: Vec<Value>,
    frontier: Vec<Value>,
    repeats: BTreeMap<String, Value>,
    max_repeats: Value,
}
impl Eval {
    fn node_state(&self, id: &str) -> &Value {
        self.state.get("nodes").get(id)
    }
    fn seq(&self, id: &str) -> Value {
        if self.node_state(id).get("seq").integer(true) {
            self.node_state(id).get("seq").clone()
        } else {
            0_i64.into()
        }
    }
    fn recorded(&self, id: &str) -> (Value, Value) {
        let state = self.node_state(id);
        let mut claimed = state.default("claimed_outcome", state.get("outcome").clone());
        if matches!(claimed, Value::Null) {
            claimed = self.state.get("gates").get(id).clone();
        }
        let recorded = state.default("outcome", claimed.clone());
        (claimed, recorded)
    }
    fn selected(&self, id: &str, outcome: &str) -> Vec<Value> {
        let matches: Vec<Value> = self
            .out
            .get(id)
            .into_iter()
            .flatten()
            .filter(|e| {
                ar(e, "outcomes").iter().any(|v| v.text() == Some(outcome))
                    && ar(e, "when")
                        .iter()
                        .all(|v| holds(v.text().unwrap_or(""), &self.facts))
            })
            .cloned()
            .collect();
        let Some(priority) = matches
            .iter()
            .map(|e| e.get("priority").clone())
            .min_by(number_cmp)
        else {
            return vec![];
        };
        let mut selected: Vec<Value> = matches
            .into_iter()
            .filter(|e| e.get("priority") == &priority)
            .collect();
        if selected.len() > 1 && !selected.iter().all(|e| e.get("fanout").truth()) {
            selected.truncate(1);
        }
        selected
    }
    fn settle(&mut self, id: &str, ancestry: &BTreeSet<String>) -> Option<String> {
        if let Some(value) = self.effective.get(id) {
            return Some(value.py());
        }
        let (claimed, recorded) = self.recorded(id);
        if matches!(claimed, Value::Null) || matches!(recorded, Value::Null) {
            return None;
        }
        let targets = self.selected(id, &recorded.py());
        let frontier = targets.is_empty()
            || targets.iter().any(|e| {
                let target = st(e, "to");
                st(self.node_state(target), "status") != "finished"
                    || ancestry.contains(target)
                    || target == id
                    || number_cmp(&self.seq(target), &self.seq(id)).is_lt()
            });
        let outcome = if frontier {
            let (outcome, absent) = effective(&self.nodes[id], &claimed.py(), &self.facts);
            if !absent.is_empty() {
                self.downgrades.push(obj([
                    ("node", id.into()),
                    ("claimed", claimed),
                    ("effective", outcome.clone().into()),
                    ("missing", list(absent)),
                ]));
            }
            outcome
        } else {
            recorded.py()
        };
        self.effective.insert(id.into(), outcome.clone().into());
        Some(outcome)
    }
    fn source_matches(&self, edge: &Value) -> bool {
        let source = st(edge, "from");
        if st(self.node_state(source), "status") != "finished" {
            return false;
        }
        let recorded = self.recorded(source).1;
        let outcome = self.effective.get(source).unwrap_or(&recorded);
        ar(edge, "outcomes").iter().any(|v| v == outcome)
            && ar(edge, "when")
                .iter()
                .all(|v| holds(v.text().unwrap_or(""), &self.facts))
    }
    fn join_ready(&self, id: &str) -> bool {
        if st(&self.nodes[id], "kind") == "terminal" {
            return true;
        }
        let edges: Vec<&Value> = self
            .ins
            .get(id)
            .into_iter()
            .flatten()
            .filter(|e| {
                st(e, "kind") == "normal"
                    && st(e, "from") != id
                    && !self.reach[id].contains(st(e, "from"))
            })
            .collect();
        if edges.len() < 2 {
            return true;
        }
        if st(&self.nodes[id], "join") == "any" {
            edges.iter().any(|e| self.source_matches(e))
        } else {
            edges.iter().all(|e| self.source_matches(e))
        }
    }
    fn push(&mut self, kind: &str, id: &str, reason: impl Into<Value>) {
        self.frontier.push(obj([
            ("kind", kind.into()),
            ("node", id.into()),
            ("reason", reason.into()),
        ]));
    }
    fn due(&mut self, id: &str) {
        if st(&self.nodes[id], "kind") == "gate" {
            self.push("gate", id, format!("{id} awaits a new gate decision"));
        } else {
            self.push("candidate", id, "");
        }
    }
    fn follow(&mut self, id: &str, outcome: &str, ancestry: &BTreeSet<String>) {
        let selected = self.selected(id, outcome);
        if selected.is_empty() {
            self.push(
                "blocked",
                id,
                format!("no route for outcome {outcome} from {id}"),
            );
            return;
        }
        for edge in selected {
            let target = st(&edge, "to");
            self.trace
                .push(format!("{id} --{outcome}--> {target}").into());
            let finished = st(self.node_state(target), "status") == "finished";
            let back = st(&edge, "kind") == "repeat" || ancestry.contains(target) || target == id;
            let mut parents = ancestry.clone();
            parents.insert(id.into());
            if finished && number_cmp(&self.seq(target), &self.seq(id)).is_gt() {
                self.expand(target, &parents);
                continue;
            }
            if back {
                let key = format!("{id}->{target}");
                let previous = self.repeats.get(&key).cloned().unwrap_or(0_i64.into());
                let next = Value::Num(oms_command_operations::value::add(&previous.py(), "1"));
                if number_cmp(&next, &self.max_repeats).is_gt() {
                    self.push(
                        "exhausted",
                        target,
                        format!("repeat budget exhausted for {key}"),
                    );
                } else {
                    self.repeats.insert(key, next);
                    self.due(target);
                }
                continue;
            }
            if finished && number_cmp(&self.seq(target), &self.seq(id)).is_lt() {
                self.due(target);
                continue;
            }
            self.expand(target, &parents);
        }
    }
    fn expand(&mut self, id: &str, ancestry: &BTreeSet<String>) {
        if ancestry.contains(id) {
            self.push("blocked", id, "route cycle reached without a repeat edge");
            return;
        }
        if !self.join_ready(id) {
            self.push(
                "join",
                id,
                format!("join {id} is waiting for incoming sources"),
            );
            return;
        }
        let node = self.nodes[id].clone();
        let state = self.node_state(id).clone();
        let decision = self.state.get("gates").get(id).clone();
        if st(&state, "status") == "active" {
            self.push("active", id, format!("{id} is active without an outcome"));
            return;
        }
        if st(&state, "status") == "finished" {
            if let Some(outcome) = self.settle(id, ancestry) {
                if st(&node, "kind") == "terminal" {
                    self.push("terminal", id, "");
                } else {
                    self.follow(id, &outcome, ancestry);
                }
            } else {
                self.push(
                    "active",
                    id,
                    format!("{id} finished without a recorded outcome"),
                );
            }
            return;
        }
        if !matches!(decision, Value::Null) {
            let (outcome, absent) = effective(&node, &decision.py(), &self.facts);
            self.effective.insert(id.into(), outcome.clone().into());
            if !absent.is_empty() {
                self.downgrades.push(obj([
                    ("node", id.into()),
                    ("claimed", decision),
                    ("effective", outcome.clone().into()),
                    ("missing", list(absent)),
                ]));
            }
            self.follow(id, &outcome, ancestry);
            return;
        }
        let absent = missing(ar(&node, "requires"), &self.facts);
        if !absent.is_empty() {
            self.push(
                "blocked",
                id,
                format!(
                    "{id} requires: {}",
                    absent.iter().map(Value::py).collect::<Vec<_>>().join(", ")
                ),
            );
            return;
        }
        match st(&node, "kind") {
            "router" => {
                self.effective.insert(id.into(), "completed".into());
                self.follow(id, "completed", ancestry);
            }
            "terminal" => self.push("terminal", id, ""),
            "gate" => self.push("gate", id, format!("{id} awaits a gate decision")),
            "subgraph" => {
                let child = self.graph.get("subgraphs").get(st(&node, "graph"));
                let child_state = self.state.get("subgraphs").get(id);
                let fallback = state_from_empty(child);
                let child_state = if matches!(child_state, Value::Obj(_)) {
                    child_state
                } else {
                    &fallback
                };
                let child_route = evaluate(child, child_state, &self.facts);
                for (key, value) in map(child_route.get("effective_outcomes")) {
                    self.effective.insert(format!("{id}.{key}"), value);
                }
                for row in ar(&child_route, "downgrades") {
                    let mut row = row.clone();
                    row.set("node", format!("{id}.{}", st(&row, "node")));
                    self.downgrades.push(row);
                }
                if st(&child_route, "status") == "terminal" {
                    self.effective.insert(id.into(), "completed".into());
                    self.follow(id, "completed", ancestry);
                } else {
                    let primary = st(&child_route, "primary");
                    let name = if primary.is_empty() {
                        id.into()
                    } else {
                        format!("{id}.{primary}")
                    };
                    self.push("subgraph", &name, child_route.get("reason").clone());
                    self.frontier.last_mut().unwrap().set("route", child_route);
                }
            }
            "agent"
                if !st(&node, "plan_task_from").is_empty()
                    && bound(&self.state, st(&node, "plan_task_from")).is_empty() =>
            {
                let name = st(&node, "plan_task_from");
                self.push("blocked", id, format!("missing task binding: {name}"));
                self.frontier.last_mut().unwrap().set(
                    "required",
                    list([obj([
                        ("kind", "task_binding".into()),
                        ("name", name.into()),
                    ])]),
                );
            }
            _ => self.push("candidate", id, ""),
        }
    }
}
fn state_from_empty(spec: &Value) -> Value {
    state(spec, &empty(), &empty(), &empty(), &empty()).unwrap_or_else(|_| empty())
}
pub fn evaluate(spec: &Value, state: &Value, facts: &Value) -> Value {
    let validation = validate(spec);
    let graph = normalize(spec).unwrap_or_else(|_| {
        obj([
            ("nodes", empty()),
            ("edges", list(Vec::<Value>::new())),
            (
                "budget",
                obj([("max_steps", 20_i64.into()), ("max_repeats", 3_i64.into())]),
            ),
        ])
    });
    let facts = binding_facts(state, facts);
    let steps = if state.get("steps").integer(false) {
        state.get("steps").clone()
    } else {
        0_i64.into()
    };
    let max_steps = graph.get("budget").default("max_steps", 20_i64);
    let max_repeats = graph.get("budget").default("max_repeats", 3_i64);
    let repeats: BTreeMap<String, Value> = map(state.get("repeats"))
        .into_iter()
        .filter(|(_, v)| v.integer(false))
        .collect();
    let mut budget = obj([
        ("steps_used", steps.clone()),
        ("max_steps", max_steps.clone()),
        ("repeats", object(repeats.clone())),
        ("max_repeats", max_repeats.clone()),
    ]);
    let result = |status: &str,
                  primary: Value,
                  alternatives: Value,
                  reason: Value,
                  required: Value,
                  gate: Value,
                  effective: Value,
                  downgrades: Value,
                  trace: Value,
                  budget: Value| {
        obj([
            ("schema", 1_i64.into()),
            ("status", status.into()),
            ("primary", primary),
            ("alternatives", alternatives),
            ("reason", reason),
            ("required_resources", required),
            ("gate", gate),
            ("effective_outcomes", effective),
            ("downgrades", downgrades),
            ("budget", budget),
            ("trace", trace),
        ])
    };
    let early = |status: &str, reason: String| {
        result(
            status,
            Value::Null,
            list(Vec::<Value>::new()),
            reason.into(),
            list(Vec::<Value>::new()),
            Value::Null,
            empty(),
            list(Vec::<Value>::new()),
            list(Vec::<Value>::new()),
            budget.clone(),
        )
    };
    if !validation.get("ok").truth() {
        return early(
            "invalid",
            format!(
                "invalid graph spec: {}",
                ar(&validation, "errors")
                    .iter()
                    .map(|v| st(v, "code"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    let stop: Vec<&str> = ar(&graph, "stop_facts")
        .iter()
        .filter_map(Value::text)
        .filter(|p| holds(p, &facts))
        .collect();
    if !stop.is_empty() {
        return early("blocked", format!("stop facts hold: {}", stop.join(", ")));
    }
    if !number_cmp(&steps, &max_steps).is_lt() {
        return early("exhausted", "step budget exhausted".into());
    }
    let nodes = map(graph.get("nodes"));
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut ins: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for edge in ar(&graph, "edges") {
        out.entry(st(edge, "from").into())
            .or_default()
            .push(edge.clone());
        ins.entry(st(edge, "to").into())
            .or_default()
            .push(edge.clone());
    }
    let mut reachability = BTreeMap::new();
    for start in nodes.keys() {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<String> = out
            .get(start)
            .into_iter()
            .flatten()
            .map(|e| st(e, "to").into())
            .collect();
        while let Some(current) = pending.pop() {
            if seen.insert(current.clone()) {
                pending.extend(
                    out.get(&current)
                        .into_iter()
                        .flatten()
                        .map(|e| st(e, "to").to_owned()),
                );
            }
        }
        reachability.insert(start.clone(), seen);
    }
    let entry = st(&graph, "entry").to_owned();
    let mut evaluator = Eval {
        graph,
        state: state.clone(),
        facts,
        nodes,
        out,
        ins,
        reach: reachability,
        effective: BTreeMap::new(),
        downgrades: vec![],
        trace: vec![],
        frontier: vec![],
        repeats,
        max_repeats,
    };
    evaluator.expand(&entry, &BTreeSet::new());
    budget.set("repeats", object(evaluator.repeats));
    let finish = |status: &str,
                  primary: Value,
                  alternatives: Value,
                  reason: Value,
                  required: Value,
                  gate: Value| {
        result(
            status,
            primary,
            alternatives,
            reason,
            required,
            gate,
            object(evaluator.effective.clone()),
            list(evaluator.downgrades.clone()),
            list(evaluator.trace.clone()),
            budget.clone(),
        )
    };
    let bucket = |kind: &str| {
        evaluator
            .frontier
            .iter()
            .filter(|v| st(v, "kind") == kind)
            .collect::<Vec<_>>()
    };
    let active = bucket("active");
    if let Some(row) = active.first() {
        return finish(
            "waiting",
            row.get("node").clone(),
            list(Vec::<Value>::new()),
            row.get("reason").clone(),
            list(Vec::<Value>::new()),
            Value::Null,
        );
    }
    let candidates: Vec<&Value> = evaluator
        .frontier
        .iter()
        .filter(|v| ["candidate", "subgraph"].contains(&st(v, "kind")))
        .collect();
    if let Some(first) = candidates.first() {
        let primary = st(first, "node");
        let mut alternatives = vec![];
        for row in candidates.iter().skip(1) {
            let node = row.get("node").clone();
            if st(row, "node") != primary && !alternatives.contains(&node) {
                alternatives.push(node);
            }
        }
        if st(first, "kind") == "subgraph" {
            let child = first.get("route");
            let parent = primary.split('.').next().unwrap_or(primary);
            let mut child_alts = ar(child, "alternatives")
                .iter()
                .map(|v| format!("{parent}.{}", v.py()).into())
                .collect::<Vec<Value>>();
            child_alts.extend(alternatives);
            return finish(
                st(child, "status"),
                primary.into(),
                list(child_alts),
                first.get("reason").clone(),
                child.get("required_resources").clone(),
                child.get("gate").clone(),
            );
        }
        let node = &evaluator.nodes[primary.split('.').next().unwrap_or(primary)];
        let mut resources = vec![];
        if st(node, "kind") == "agent" {
            let source = st(node, "plan_task_from");
            let task = if source.is_empty() {
                st(node, "plan_task").to_owned()
            } else {
                bound(&evaluator.state, source)
            };
            if !task.is_empty() {
                let mut resource = obj([("kind", "plan_task".into()), ("id", task.clone().into())]);
                if !source.is_empty() {
                    resource.set("binding", source);
                } else if task == "next" {
                    resource.set("selector", true);
                }
                resources.push(resource);
            }
        }
        return finish(
            "actionable",
            primary.into(),
            list(alternatives),
            format!("{primary} is actionable").into(),
            list(resources),
            Value::Null,
        );
    }
    let gates = bucket("gate");
    if let Some(first) = gates.first() {
        let id = st(first, "node");
        let node = &evaluator.nodes[id];
        let gate = obj([
            ("node", id.into()),
            ("authority", node.default("authority", "parent")),
            ("decisions", node.get("decisions").clone()),
        ]);
        return finish(
            "gate",
            id.into(),
            list(gates.iter().skip(1).map(|r| r.get("node").clone())),
            first.get("reason").clone(),
            list(Vec::<Value>::new()),
            gate,
        );
    }
    let exhausted = bucket("exhausted");
    let terminal = bucket("terminal");
    let joins = bucket("join");
    let blocked = bucket("blocked");
    let (status, row) = if let Some(row) = exhausted.first() {
        ("exhausted", Some(*row))
    } else if !terminal.is_empty() && joins.is_empty() && blocked.is_empty() {
        ("terminal", Some(terminal[0]))
    } else if !joins.is_empty() && blocked.is_empty() {
        ("waiting", Some(joins[0]))
    } else if !blocked.is_empty() {
        ("blocked", Some(blocked[0]))
    } else if !terminal.is_empty() {
        ("terminal", Some(terminal[0]))
    } else {
        ("blocked", None)
    };
    if let Some(row) = row {
        let reason = if status == "terminal" {
            format!("terminal {} reached", st(row, "node")).into()
        } else {
            row.get("reason").clone()
        };
        finish(
            status,
            row.get("node").clone(),
            list(Vec::<Value>::new()),
            reason,
            row.default("required", list(Vec::<Value>::new())),
            Value::Null,
        )
    } else {
        finish(
            "blocked",
            Value::Null,
            list(Vec::<Value>::new()),
            "no actionable route".into(),
            list(Vec::<Value>::new()),
            Value::Null,
        )
    }
}
pub fn fixture(fixture: &Value, cwd: &Path) -> Result<Value> {
    fixture_with_order(fixture, cwd, None)
}
pub fn fixture_with_order(
    fixture: &Value,
    cwd: &Path,
    expected_keys: Option<&[Vec<u8>]>,
) -> Result<Value> {
    if !matches!(fixture, Value::Obj(_)) {
        return Err(Error::new("route fixture must be an object"));
    }
    let spec = if fixture.has("spec") {
        load(fixture.get("spec"), cwd)?
    } else if fixture.get("spec_ref").truth() {
        load(fixture.get("spec_ref"), cwd)?
    } else {
        return Err(Error::new("route fixture requires spec or spec_ref"));
    };
    for key in ["facts", "outcomes", "gates", "repeats", "bindings"] {
        if fixture.has(key) && !matches!(fixture.get(key), Value::Obj(_)) {
            return Err(Error::new(
                "fixture facts, outcomes, gates, repeats, and bindings must be objects",
            ));
        }
    }
    let mut state = state(
        &spec,
        fixture.get("outcomes"),
        fixture.get("gates"),
        fixture.get("repeats"),
        fixture.get("bindings"),
    )?;
    if fixture.get("steps").integer(false) {
        state.set("steps", fixture.get("steps").clone());
    }
    let actual_route = evaluate(&spec, &state, fixture.get("facts"));
    let expected = if matches!(fixture.get("expect"), Value::Obj(_)) {
        fixture.get("expect").clone()
    } else {
        empty()
    };
    let mut actual = BTreeMap::new();
    let mut diff = vec![];
    let Value::Obj(fields) = &expected else {
        unreachable!()
    };
    let keys: Vec<_> = expected_keys
        .map(|keys| keys.to_vec())
        .unwrap_or_else(|| fields.keys().cloned().collect());
    for key in keys {
        let Some(wanted) = fields.get(&key) else {
            continue;
        };
        let got = if key == b"downgrades" {
            list(
                ar(&actual_route, "downgrades")
                    .iter()
                    .map(|row| row.get("node").clone()),
            )
        } else if let Value::Obj(route) = &actual_route {
            route.get(&key).cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        actual.insert(key.clone(), got.clone());
        if got != *wanted {
            diff.push(obj([
                ("key", Value::Str(key)),
                ("expected", wanted.clone()),
                ("actual", got),
            ]));
        }
    }
    Ok(obj([
        ("expected", expected),
        ("actual", Value::Obj(actual)),
        ("diff", list(diff)),
    ]))
}
