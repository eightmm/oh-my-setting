use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
pub fn route_text(route: &Value) -> String {
    let mut text = format!(
        "{} {} {}\n",
        st(route, "status"),
        route.get("primary").or("-").py(),
        st(route, "reason")
    );
    for row in ar(route, "alternatives") {
        text.push_str(&format!("alternative: {}\n", row.py()));
    }
    for row in ar(route, "downgrades") {
        text.push_str(&format!(
            "downgrade: {} claimed={} effective={} missing={}\n",
            st(row, "node"),
            st(row, "claimed"),
            st(row, "effective"),
            ar(row, "missing")
                .iter()
                .map(Value::py)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    text
}
fn label(text: &str) -> String {
    text.replace('\\', "/").replace('"', "'").replace('\n', " ")
}
fn spec_edges(spec: &Value, nodes: &BTreeMap<String, Value>) -> Vec<Value> {
    let mut edges: Vec<_> = ar(spec, "edges")
        .iter()
        .filter(|edge| nodes.contains_key(st(edge, "from")) && nodes.contains_key(st(edge, "to")))
        .cloned()
        .map(|mut edge| {
            let priority =
                oms_command_operations::value::integer_value(&edge.get("priority").or(0_i64))
                    .unwrap_or(0_i64.into());
            edge.set("priority", priority);
            if !["normal", "repeat"].contains(&st(&edge, "kind")) {
                edge.set("kind", "normal");
            }
            edge
        })
        .collect();
    edges.sort_by(|a, b| {
        number_cmp(a.get("priority"), b.get("priority")).then_with(|| {
            (
                st(a, "from"),
                st(a, "to"),
                ar(a, "outcomes")
                    .iter()
                    .map(Value::py)
                    .collect::<Vec<_>>()
                    .join(","),
            )
                .cmp(&(
                    st(b, "from"),
                    st(b, "to"),
                    ar(b, "outcomes")
                        .iter()
                        .map(Value::py)
                        .collect::<Vec<_>>()
                        .join(","),
                ))
        })
    });
    edges
}
fn kind(node: &Value) -> &str {
    if st(node, "kind").is_empty() {
        "node"
    } else {
        st(node, "kind")
    }
}
pub fn exec(
    spec: &Value,
    projection: Option<&Value>,
    route: Option<&Value>,
    mermaid: bool,
) -> String {
    let nodes = map(spec.get("nodes"));
    let edges = spec_edges(spec, &nodes);
    if mermaid {
        let mut ids = BTreeMap::new();
        let mut taken = BTreeSet::new();
        for id in nodes.keys() {
            let mut safe = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            if safe.is_empty() || safe.starts_with(|c: char| c.is_ascii_digit()) {
                safe = format!("n_{safe}");
            }
            let mut candidate = safe.clone();
            let mut suffix = 2;
            while taken.contains(&candidate) {
                candidate = format!("{safe}_{suffix}");
                suffix += 1;
            }
            taken.insert(candidate.clone());
            ids.insert(id.clone(), candidate);
        }
        let mut out = "flowchart TD\n".to_owned();
        for (id, node) in &nodes {
            out.push_str(&format!(
                "    {}[\"{}\\n({})\"]\n",
                ids[id],
                label(id),
                label(kind(node))
            ));
        }
        for edge in &edges {
            let outcomes = ar(edge, "outcomes")
                .iter()
                .map(Value::py)
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "    {} {}{} {}\n",
                ids[st(edge, "from")],
                if st(edge, "kind") == "repeat" {
                    "-.->"
                } else {
                    "-->"
                },
                if outcomes.is_empty() {
                    String::new()
                } else {
                    format!("|\"{}\"|", label(&outcomes))
                },
                ids[st(edge, "to")]
            ));
        }
        if let Some(projection) = projection {
            for (name, colors) in [
                ("done", "fill:#dff0d8,stroke:#3c763d,color:#1b1b1b"),
                ("active", "fill:#fcf8e3,stroke:#8a6d3b,color:#1b1b1b"),
                ("failed", "fill:#f2dede,stroke:#a94442,color:#1b1b1b"),
            ] {
                out.push_str(&format!("    classDef {name} {colors};\n"));
            }
            for bucket in ["done", "active", "failed"] {
                let ids: Vec<String> = nodes
                    .keys()
                    .filter(|id| {
                        let state = projection.get("nodes").get(id);
                        match bucket {
                            "done" => {
                                st(state, "status") == "finished"
                                    && ["completed", "approved"].contains(&st(state, "outcome"))
                            }
                            "failed" => {
                                st(state, "status") == "finished"
                                    && ["failed", "blocked", "unverified"]
                                        .contains(&st(state, "outcome"))
                            }
                            _ => st(state, "status") == "active",
                        }
                    })
                    .map(|id| ids[id].clone())
                    .collect();
                if !ids.is_empty() {
                    out.push_str(&format!("    class {} {bucket};\n", ids.join(",")));
                }
            }
        }
        return out;
    }
    let entry = st(spec, "entry");
    let order = render_order(&nodes, &edges, entry);
    let mut out = format!(
        "graph {} (entry {})\n",
        spec.get("id").or("-").py(),
        if entry.is_empty() { "-" } else { entry }
    );
    if let Some(route) = route {
        if !st(route, "status").is_empty() {
            out.push_str(&format!(
                "route {}{}\n",
                st(route, "status"),
                if st(route, "reason").is_empty() {
                    String::new()
                } else {
                    format!(": {}", st(route, "reason"))
                }
            ));
        }
    }
    for id in order {
        let node = &nodes[&id];
        let state = projection
            .map(|p| p.get("nodes").get(&id))
            .unwrap_or(&oms_command_operations::value::NULL);
        let primary = route.is_some_and(|r| st(r, "primary") == id);
        let glyph = if st(state, "status") == "finished"
            && ["completed", "approved"].contains(&st(state, "outcome"))
        {
            "✔"
        } else if st(state, "status") == "finished"
            && ["failed", "blocked", "unverified"].contains(&st(state, "outcome"))
        {
            "✖"
        } else if st(node, "kind") == "gate"
            && projection.is_none_or(|p| !p.get("gates").has(&id))
            && (st(state, "status") == "active" || primary)
        {
            "⏸"
        } else if st(state, "status") == "active" || primary {
            "◇"
        } else {
            "○"
        };
        let mut line = format!("{glyph} {id} ({})", kind(node));
        if !st(state, "outcome").is_empty() {
            line.push_str(&format!(" [{}]", st(state, "outcome")));
        }
        line.push_str(&binding_label(node, state, projection));
        let title = st(node, "title");
        if !title.is_empty() && title != id {
            line.push_str(&format!(" — {title}"));
        }
        if primary {
            line.push_str("  ← next");
        }
        out.push_str(&line);
        out.push('\n');
        let outgoing: Vec<&Value> = edges.iter().filter(|e| st(e, "from") == id).collect();
        for (i, edge) in outgoing.iter().enumerate() {
            let outcomes = ar(edge, "outcomes")
                .iter()
                .map(Value::py)
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "  {} {} → {}{}\n",
                if i + 1 == outgoing.len() {
                    "└──"
                } else {
                    "├──"
                },
                if outcomes.is_empty() { "-" } else { &outcomes },
                st(edge, "to"),
                if st(edge, "kind") == "repeat" {
                    " ↻"
                } else {
                    ""
                }
            ));
        }
    }
    out
}
fn render_order(nodes: &BTreeMap<String, Value>, edges: &[Value], entry: &str) -> Vec<String> {
    let mut adjacency: BTreeMap<String, BTreeSet<String>> = nodes
        .keys()
        .map(|id| (id.clone(), BTreeSet::new()))
        .collect();
    let mut indegree: BTreeMap<String, usize> = nodes.keys().map(|id| (id.clone(), 0)).collect();
    for edge in edges {
        if adjacency
            .get_mut(st(edge, "from"))
            .unwrap()
            .insert(st(edge, "to").into())
        {
            *indegree.get_mut(st(edge, "to")).unwrap() += 1;
        }
    }
    let mut distance = BTreeMap::new();
    let mut queue = VecDeque::new();
    if nodes.contains_key(entry) {
        distance.insert(entry.to_owned(), 0);
        queue.push_back(entry.to_owned());
    }
    while let Some(id) = queue.pop_front() {
        let next = distance[&id] + 1;
        for target in &adjacency[&id] {
            if !distance.contains_key(target) {
                distance.insert(target.clone(), next);
                queue.push_back(target.clone());
            }
        }
    }
    let mut remaining: BTreeSet<String> = nodes.keys().cloned().collect();
    let mut order = vec![];
    while !remaining.is_empty() {
        let chosen = if order.is_empty() && remaining.contains(entry) {
            entry.to_owned()
        } else {
            remaining
                .iter()
                .find(|id| indegree[*id] == 0)
                .cloned()
                .unwrap_or_else(|| {
                    remaining
                        .iter()
                        .min_by_key(|id| {
                            (distance.get(*id).copied().unwrap_or(nodes.len() + 1), *id)
                        })
                        .unwrap()
                        .clone()
                })
        };
        remaining.remove(&chosen);
        for target in &adjacency[&chosen] {
            *indegree.get_mut(target).unwrap() -= 1;
        }
        order.push(chosen);
    }
    order
}
fn binding_label(node: &Value, state: &Value, projection: Option<&Value>) -> String {
    if st(node, "kind") != "agent" || projection.is_none() {
        return String::new();
    }
    if !st(node, "bind_task").is_empty() && !st(state, "task_id").is_empty() {
        format!(" [{}={}]", st(node, "bind_task"), st(state, "task_id"))
    } else if !st(node, "plan_task_from").is_empty() {
        let held = bound(projection.unwrap(), st(node, "plan_task_from"));
        format!(
            " [task={}→{}]",
            st(node, "plan_task_from"),
            if held.is_empty() { "?" } else { &held }
        )
    } else if !st(state, "task_id").is_empty() {
        format!(" [task={}]", st(state, "task_id"))
    } else {
        String::new()
    }
}

pub fn visual_model(
    spec: &Value,
    projection: Option<&Value>,
    route: Option<&Value>,
) -> serde_json::Value {
    use serde_json::json;
    let nodes = map(spec.get("nodes"));
    let edges = spec_edges(spec, &nodes);
    let mut degree: BTreeMap<String, usize> = nodes.keys().map(|id| (id.clone(), 0)).collect();
    for edge in &edges {
        *degree.get_mut(st(edge, "from")).unwrap() += 1;
        *degree.get_mut(st(edge, "to")).unwrap() += 1;
    }
    let rows:Vec<_>=render_order(&nodes,&edges,st(spec,"entry")).into_iter().map(|id| {
        let node=&nodes[&id];
        let state=projection.map(|p|p.get("nodes").get(&id)).unwrap_or(&oms_command_operations::value::NULL);
        let title=st(node,"title");
        json!({"id":id,"label":if !title.is_empty() && title!=id{title}else{&id},"kind":if st(node,"kind").is_empty(){"node"}else{st(node,"kind")},"language":"","path":"","degree":degree[&id],"focus":route.is_some_and(|r|st(r,"primary")==id),"show_label":true,"status":if st(state,"status").is_empty(){"pending"}else{st(state,"status")},"outcome":st(state,"outcome"),"binding":binding_label(node,state,projection).trim()})
    }).collect();
    let edge_rows:Vec<_>=edges.iter().map(|edge| {
        let outcomes=ar(edge,"outcomes").iter().map(Value::py).collect::<Vec<_>>().join(", ");
        json!({"source":st(edge,"from"),"target":st(edge,"to"),"relation":if outcomes.is_empty(){"next"}else{&outcomes},"confidence":if st(edge,"kind")=="repeat"{"REPEAT"}else{"NORMAL"}})
    }).collect();
    json!({"schema":1,"kind":"execution","revision":st(spec,"id"),"nodes":rows,"edges":edge_rows,"counts":{"shown_nodes":rows.len(),"total_nodes":rows.len(),"shown_edges":edge_rows.len(),"total_edges":edge_rows.len()}})
}
pub fn html(spec: &Value, projection: Option<&Value>, route: Option<&Value>) -> String {
    let id = st(spec, "id");
    crate::project::visual::html(
        &visual_model(spec, projection, route),
        &format!(
            "OMS Execution Graph · {}",
            if id.is_empty() { "execution" } else { id }
        ),
    )
}
