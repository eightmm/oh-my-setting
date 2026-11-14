use super::{analytics, index::Graph};
use crate::util::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn confidence(edge: &Value) -> u8 {
    match s(edge, "confidence") {
        "EXTRACTED" => 0,
        "INFERRED" => 1,
        "AMBIGUOUS" => 2,
        _ => 3,
    }
}
pub fn model(
    graph: &Graph,
    limit: i64,
    depth: i64,
    include_tests: bool,
    coverage: &Value,
    focus: &[String],
    paths: &[String],
    changed: &[String],
    impacted: &[String],
) -> Value {
    let cap = limit.clamp(1, 200) as usize;
    let radius = depth.clamp(0, 4) as u64;
    let ids: BTreeSet<&str> = graph
        .nodes
        .keys()
        .filter(|id| include_tests || !graph.is_test(id))
        .map(String::as_str)
        .collect();
    let raw: Vec<&Value> = array(&graph.graph, "edges")
        .iter()
        .filter(|e| {
            graph.nodes.contains_key(s(e, "source")) && graph.nodes.contains_key(s(e, "target"))
        })
        .collect();
    let valid: Vec<&Value> = raw
        .iter()
        .copied()
        .filter(|e| ids.contains(s(e, "source")) && ids.contains(s(e, "target")))
        .collect();
    let degrees = analytics::degrees(graph, &ids);
    let degree = |id: &str| degrees.get(id).copied().unwrap_or_default().2;
    let assurance = analytics::assurance(graph, include_tests);
    let primary: BTreeSet<&str> = focus
        .iter()
        .map(String::as_str)
        .filter(|id| ids.contains(id))
        .collect();
    let context: BTreeSet<&str> = ids
        .iter()
        .copied()
        .filter(|id| {
            paths.iter().any(|p| p == s(&graph.nodes[*id], "path"))
                && ["file", "module", "test"].contains(&s(&graph.nodes[*id], "kind"))
        })
        .collect();
    let mut ranked: Vec<&str> = ids.iter().copied().collect();
    ranked.sort_by_key(|id| (std::cmp::Reverse(degree(id)), *id));
    let mut roots: Vec<&str> = primary.union(&context).copied().collect();
    roots.sort_by_key(|id| (std::cmp::Reverse(degree(id)), *id));
    if roots.is_empty() {
        roots = ranked.iter().take(cap.min(12)).copied().collect();
    }
    let mut adjacency: BTreeMap<&str, Vec<(&str, &Value)>> =
        ids.iter().map(|id| (*id, vec![])).collect();
    for edge in &valid {
        adjacency
            .get_mut(s(edge, "source"))
            .unwrap()
            .push((s(edge, "target"), edge));
        adjacency
            .get_mut(s(edge, "target"))
            .unwrap()
            .push((s(edge, "source"), edge));
    }
    let mut distance: BTreeMap<&str, u64> = roots.iter().map(|id| (*id, 0)).collect();
    let mut queue: VecDeque<&str> = roots.into_iter().collect();
    let mut selected = vec![];
    while let Some(id) = queue.pop_front() {
        if selected.len() >= cap {
            break;
        }
        selected.push(id);
        if distance[id] >= radius {
            continue;
        }
        let mut neighbors = adjacency[id].clone();
        neighbors.sort_by_key(|(id, e)| (confidence(e), std::cmp::Reverse(degree(id)), *id));
        for (adjacent, _) in neighbors {
            if !distance.contains_key(adjacent) {
                distance.insert(adjacent, distance[id] + 1);
                queue.push_back(adjacent);
            }
        }
    }
    if selected.len() < cap {
        for id in &ranked {
            if distance.contains_key(id) {
                continue;
            }
            distance.insert(id, radius + 1);
            selected.push(id);
            if selected.len() >= cap {
                break;
            }
        }
    }
    let kept: BTreeSet<&str> = selected.iter().copied().collect();
    let mut task_distance: BTreeMap<&str, u64> = primary.iter().map(|id| (*id, 0)).collect();
    let mut queue: VecDeque<&str> = primary.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if task_distance[id] >= radius {
            continue;
        }
        for (adjacent, edge) in &adjacency[id] {
            if kept.contains(adjacent)
                && s(edge, "confidence") == "EXTRACTED"
                && !task_distance.contains_key(adjacent)
            {
                task_distance.insert(adjacent, task_distance[id] + 1);
                queue.push_back(adjacent);
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut edges = vec![];
    for edge in &valid {
        let key = (
            s(edge, "source"),
            s(edge, "target"),
            s(edge, "relation"),
            s(edge, "confidence"),
        );
        if kept.contains(key.0) && kept.contains(key.1) && seen.insert(key) {
            edges.push(json!({"source":key.0,"target":key.1,"relation":key.2,"confidence":key.3}));
        }
    }
    edges.sort_by(|a, b| {
        (
            confidence(a),
            distance
                .get(s(a, "source"))
                .copied()
                .unwrap_or(radius + 2)
                .min(distance.get(s(a, "target")).copied().unwrap_or(radius + 2)),
            s(a, "source"),
            s(a, "target"),
            s(a, "relation"),
        )
            .cmp(&(
                confidence(b),
                distance
                    .get(s(b, "source"))
                    .copied()
                    .unwrap_or(radius + 2)
                    .min(distance.get(s(b, "target")).copied().unwrap_or(radius + 2)),
                s(b, "source"),
                s(b, "target"),
                s(b, "relation"),
            ))
    });
    edges.truncate(40.max(cap * 3));
    let mut labels: BTreeSet<&str> = ranked.iter().take(cap.min(6)).copied().collect();
    let mut primary_labels: Vec<&str> = primary.iter().copied().collect();
    primary_labels.sort_by_key(|id| (std::cmp::Reverse(degree(id)), *id));
    labels.extend(primary_labels.into_iter().take(cap.min(8)));
    let mut attention: Vec<&str> = selected
        .iter()
        .copied()
        .filter(|id| s(&assurance["nodes"][*id], "assurance") == "attention")
        .collect();
    attention.sort_by_key(|id| (std::cmp::Reverse(degree(id)), *id));
    labels.extend(attention.into_iter().take(cap.min(4)));
    let mut assurance_counts = BTreeMap::<String, usize>::new();
    let mut change_counts = BTreeMap::<String, usize>::new();
    let nodes:Vec<Value>=selected.iter().map(|id|{let node=&graph.nodes[*id];let path=s(node,"path");let change=if changed.iter().any(|p|p==path){"changed"}else if impacted.iter().any(|p|p==path){"impacted"}else{""};let mut row=json!({"id":id,"label":if s(node,"name").is_empty(){*id}else{s(node,"name")},"kind":node["kind"],"language":node["language"],"path":path,"degree":degree(id),"focus":primary.contains(id),"distance":task_distance.get(id),"change":change,"show_label":labels.contains(id),"line":node["metadata"]["line"]});if let Some(details)=assurance["nodes"][*id].as_object(){row.as_object_mut().unwrap().extend(details.clone());}*assurance_counts.entry(s(&row,"assurance").into()).or_default()+=1;if !change.is_empty(){*change_counts.entry(change.into()).or_default()+=1;}row}).collect();
    let coverage_view = if coverage.is_object() {
        json!({"parsed":n(coverage,"parsed"),"unparsed":n(coverage,"unparsed"),"unparsed_by_extension":coverage["unparsed_by_extension"]})
    } else {
        json!({})
    };
    let hidden = graph.nodes.keys().filter(|id| graph.is_test(id)).count();
    json!({"schema":3,"kind":"project","revision":graph.graph["revision"],"coverage":coverage_view,"nodes":nodes,"edges":edges,"display":{"production_only":!include_tests,"assurance_basis":assurance["basis"],"assurance_note":assurance["note"],"default_scope":if primary.is_empty(){"all"}else{"neighbors"},"task_focus":!primary.is_empty(),"change_overlay":!changed.is_empty()||!impacted.is_empty()},"counts":{"shown_nodes":nodes.len(),"total_nodes":ids.len(),"shown_edges":edges.len(),"total_edges":valid.len(),"hidden_test_nodes":if include_tests{0}else{hidden},"hidden_test_edges":if include_tests{0}else{raw.len()-valid.len()},"assurance":assurance_counts,"change":change_counts}})
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
pub fn html(value: &Value, title: &str) -> String {
    let raw = bytes(value, false).unwrap_or_default();
    let encoded = String::from_utf8_lossy(&raw)
        .trim_end_matches('\n')
        .to_owned();
    let digest = sha(format!("{title}\0{encoded}"));
    let root = format!("oms-graph-view-{}", &digest[..12]);
    let counts = &value["counts"];
    let mut summary = format!(
        "{}/{} nodes · {}/{} edges",
        n(counts, "shown_nodes"),
        n(counts, "total_nodes"),
        n(counts, "shown_edges"),
        n(counts, "total_edges")
    );
    let revision = s(value, "revision");
    if !revision.is_empty() {
        summary.push_str(&format!(
            " · {} {}",
            if s(value, "kind") == "project" {
                "revision"
            } else {
                "spec"
            },
            revision.chars().take(12).collect::<String>()
        ));
    }
    if s(value, "kind") == "project" && value["display"]["production_only"] == true {
        summary.push_str(&format!(
            " · production only · {} hidden test nodes as evidence",
            n(counts, "hidden_test_nodes")
        ));
    }
    if s(value, "kind") == "project" && value["coverage"].as_object().is_some_and(|c| !c.is_empty())
    {
        summary.push_str(&format!(
            " · parsed {} / unparsed {}",
            n(&value["coverage"], "parsed"),
            n(&value["coverage"], "unparsed")
        ));
    }
    let mut items = String::new();
    for row in array(value, "nodes") {
        let mut location = s(row, "path").to_owned();
        if let Some(line) = row["line"].as_i64() {
            location.push_str(&format!(":{line}"));
        }
        let state = [
            s(row, "assurance_label"),
            s(row, "status"),
            s(row, "kind"),
            "node",
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap();
        let detail = [s(row, "kind"), &location, state]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        items.push_str(&format!(
            "<li><code>{}</code>{}</li>",
            escape(s(row, "id")),
            if detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", escape(&detail))
            }
        ));
    }
    let fallback = format!(
        "<details class=\"oms-graph-fallback\"><summary id=\"{root}-fallback-summary\">Graph nodes (text alternative) — {}</summary><ul id=\"{root}-fallback-list\">{items}</ul></details>",
        array(value, "nodes").len()
    );
    include_str!("graph.html")
        .replace("@@ROOT@@", &root)
        .replace("@@TITLE@@", &escape(title))
        .replace("@@SUMMARY@@", &escape(&summary))
        .replace("@@FALLBACK@@", &fallback)
        .replace(
            "@@DATA@@",
            &encoded
                .replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e"),
        )
}
