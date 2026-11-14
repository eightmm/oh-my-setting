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
pub fn exec(
    spec: &Value,
    projection: Option<&Value>,
    route: Option<&Value>,
    mermaid: bool,
) -> String {
    let nodes = map(spec.get("nodes"));
    let mut edges: Vec<Value> = ar(spec, "edges")
        .iter()
        .filter(|e| nodes.contains_key(st(e, "from")) && nodes.contains_key(st(e, "to")))
        .cloned()
        .collect();
    edges.sort_by(|a, b| {
        (
            count(a, "priority"),
            st(a, "from"),
            st(a, "to"),
            ar(a, "outcomes")
                .iter()
                .map(Value::py)
                .collect::<Vec<_>>()
                .join(","),
        )
            .cmp(&(
                count(b, "priority"),
                st(b, "from"),
                st(b, "to"),
                ar(b, "outcomes")
                    .iter()
                    .map(Value::py)
                    .collect::<Vec<_>>()
                    .join(","),
            ))
    });
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
                label(st(node, "kind"))
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
    let mut adjacency: BTreeMap<String, BTreeSet<String>> = nodes
        .keys()
        .map(|id| (id.clone(), BTreeSet::new()))
        .collect();
    let mut indegree: BTreeMap<String, usize> = nodes.keys().map(|id| (id.clone(), 0)).collect();
    for edge in &edges {
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
        let mut line = format!("{glyph} {id} ({})", st(node, "kind"));
        if !st(state, "outcome").is_empty() {
            line.push_str(&format!(" [{}]", st(state, "outcome")));
        }
        if st(node, "kind") == "agent" && projection.is_some() {
            if !st(node, "bind_task").is_empty() && !st(state, "task_id").is_empty() {
                line.push_str(&format!(
                    " [{}={}]",
                    st(node, "bind_task"),
                    st(state, "task_id")
                ));
            } else if !st(node, "plan_task_from").is_empty() {
                let held = bound(projection.unwrap(), st(node, "plan_task_from"));
                line.push_str(&format!(
                    " [task={}→{}]",
                    st(node, "plan_task_from"),
                    if held.is_empty() { "?" } else { &held }
                ));
            } else if !st(state, "task_id").is_empty() {
                line.push_str(&format!(" [task={}]", st(state, "task_id")));
            }
        }
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
pub fn html(value: &Value) -> String {
    let raw = value.json(false).unwrap_or_default();
    let data = String::from_utf8_lossy(&raw)
        .replace('<', "\\u003c")
        .replace('&', "\\u0026");
    format!(
        "<section aria-label=\"OMS Execution Graph\"><pre id=\"oms-graph-view\"></pre><script type=\"application/json\" id=\"oms-graph-data\">{data}</script><script>document.getElementById('oms-graph-view').textContent=JSON.stringify(JSON.parse(document.getElementById('oms-graph-data').textContent),null,2)</script></section>"
    )
}
