use super::index::Graph;
use crate::util::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn site(edge: &Value, graph: &Graph) -> String {
    let target = s(edge, "target");
    let discriminator = if s(edge, "confidence") == "AMBIGUOUS" {
        graph.nodes.get(target).map(|n| s(n, "name")).unwrap_or("")
    } else {
        target
    };
    format!(
        "{}\0{}\0{}\0{}\0{}",
        s(edge, "source"),
        s(edge, "relation"),
        s(&edge["evidence"], "path"),
        edge["evidence"]["line"],
        discriminator
    )
}
pub fn degrees(graph: &Graph, ids: &BTreeSet<&str>) -> BTreeMap<String, (usize, usize, usize)> {
    let mut counts: BTreeMap<String, (usize, usize, usize)> =
        ids.iter().map(|id| ((*id).into(), (0, 0, 0))).collect();
    let mut outgoing = BTreeSet::new();
    let mut incoming = BTreeSet::new();
    for edge in array(&graph.graph, "edges") {
        let source = s(edge, "source");
        let target = s(edge, "target");
        if !ids.contains(source) || !ids.contains(target) {
            continue;
        }
        let occurrence = format!(
            "{source}\0{target}\0{}\0{}\0{}\0{}",
            s(edge, "relation"),
            s(edge, "confidence"),
            edge["evidence"]["path"],
            edge["evidence"]["line"]
        );
        let out = if s(edge, "confidence") == "AMBIGUOUS" {
            site(edge, graph)
        } else {
            occurrence.clone()
        };
        if outgoing.insert(out) {
            counts.get_mut(source).unwrap().1 += 1;
        }
        if incoming.insert(occurrence) {
            counts.get_mut(target).unwrap().0 += 1;
        }
    }
    for value in counts.values_mut() {
        value.2 = value.0 + value.1;
    }
    counts
}
pub fn assurance(graph: &Graph, include_tests: bool) -> Value {
    let mut path_tests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut node_tests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut uncertain: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for edge in array(&graph.graph, "edges") {
        let source = s(edge, "source");
        let target = s(edge, "target");
        let (Some(left), Some(right)) = (graph.nodes.get(source), graph.nodes.get(target)) else {
            continue;
        };
        let st = graph.is_test(source);
        let tt = graph.is_test(target);
        let confidence = s(edge, "confidence");
        if st != tt && confidence == "EXTRACTED" {
            let (prod, test) = if st { (right, left) } else { (left, right) };
            let test_path = s(test, "path");
            if !test_path.is_empty() {
                if !s(prod, "path").is_empty() {
                    path_tests
                        .entry(s(prod, "path").into())
                        .or_default()
                        .insert(test_path.into());
                } else if s(prod, "kind") == "env" && s(edge, "relation") == "reads_env" {
                    node_tests
                        .entry(s(prod, "id").into())
                        .or_default()
                        .insert(test_path.into());
                }
            }
        }
        if !st && !tt && ["INFERRED", "AMBIGUOUS"].contains(&confidence) {
            let bucket = uncertain.entry(source.into()).or_default();
            if confidence == "INFERRED" {
                bucket.0.insert(site(edge, graph));
            } else {
                bucket.1.insert(site(edge, graph));
            }
        }
    }
    let mut nodes = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (id, node) in &graph.nodes {
        let test = graph.is_test(id);
        if test && !include_tests {
            continue;
        }
        let mut linked = path_tests.get(s(node, "path")).cloned().unwrap_or_default();
        linked.extend(node_tests.get(id).into_iter().flatten().cloned());
        let (inferred, ambiguous) = uncertain
            .get(id)
            .map(|r| (r.0.len(), r.1.len()))
            .unwrap_or_default();
        let (state, label, reason) = if test {
            (
                "test-evidence",
                "Test evidence",
                "Explicitly included test node".into(),
            )
        } else if !linked.is_empty() {
            (
                "supported",
                "Test-linked",
                format!(
                    "{} extracted test-file link(s); pass status is not asserted{}",
                    linked.len(),
                    if inferred + ambiguous > 0 {
                        format!("; {} unresolved site(s) remain", inferred + ambiguous)
                    } else {
                        String::new()
                    }
                ),
            )
        } else if ambiguous > 0 {
            (
                "attention",
                "Attention",
                format!("No extracted test-file link; {ambiguous} ambiguous source site(s)"),
            )
        } else {
            (
                "needs-evidence",
                "Needs evidence",
                format!(
                    "No extracted test-file link in this graph{}",
                    if inferred > 0 {
                        format!("; {inferred} inferred source site(s) remain")
                    } else {
                        String::new()
                    }
                ),
            )
        };
        let signals = if test {
            json!({"extracted_test_links":0,"extracted_test_paths":[],"omitted_test_paths":0,"inferred_sites":0,"ambiguous_sites":0})
        } else {
            json!({"extracted_test_links":linked.len(),"extracted_test_paths":linked.iter().take(12).collect::<Vec<_>>(),"omitted_test_paths":linked.len().saturating_sub(12),"inferred_sites":inferred,"ambiguous_sites":ambiguous})
        };
        nodes.insert(id.clone(),json!({"assurance":state,"assurance_label":label,"assurance_reason":reason,"signals":signals}));
        *counts.entry(state.into()).or_default() += 1;
    }
    json!({"schema":1,"basis":"structural-evidence","note":"Structural evidence only; no test-pass or completion claim.","nodes":nodes,"counts":counts})
}

pub fn recall_summary(rows: &[Value]) -> Value {
    let count = rows.len();
    let recall: f64 = rows
        .iter()
        .map(|row| row["recall_at_8"].as_f64().unwrap_or(0.0))
        .sum();
    let hits = rows
        .iter()
        .filter(|row| !array(row, "hits").is_empty())
        .count();
    let mut sizes: Vec<_> = rows.iter().map(|row| n(row, "pack_bytes")).collect();
    sizes.sort_unstable();
    let median = if count == 0 {
        json!(0)
    } else if count % 2 == 1 {
        json!(sizes[count / 2])
    } else {
        json!((sizes[count / 2 - 1] as f64 + sizes[count / 2] as f64) / 2.0)
    };
    json!({"cases":count,"recall_at_8":if count==0 {0.0}else{recall/count as f64},"hit_at_8":if count==0{0.0}else{hits as f64/count as f64},"median_pack_bytes":median})
}
pub fn recall_compare(left: &Value, right: &Value) -> crate::Result<Value> {
    let cases = |value: &Value| -> crate::Result<BTreeMap<String, Value>> {
        array(value, "cases")
            .iter()
            .map(|row| {
                let commit = row["commit"]
                    .as_str()
                    .ok_or_else(|| crate::Error::new("recall snapshot case lacks commit"))?;
                if !row["hits"].is_number() {
                    return Err(crate::Error::new("recall snapshot case lacks numeric hits"));
                }
                Ok((commit.to_owned(), row.clone()))
            })
            .collect()
    };
    let before = cases(left)?;
    let after = cases(right)?;
    let shared: Vec<_> = before
        .keys()
        .filter(|key| after.contains_key(*key))
        .collect();
    let comparable = before.keys().eq(after.keys()) && left["definition"] == right["definition"];
    let improved: Vec<_> = shared
        .iter()
        .filter(|key| {
            after[key.as_str()]["hits"].as_f64().unwrap_or(0.0)
                > before[key.as_str()]["hits"].as_f64().unwrap_or(0.0)
        })
        .collect();
    let regressed: Vec<_> = shared
        .iter()
        .filter(|key| {
            after[key.as_str()]["hits"].as_f64().unwrap_or(0.0)
                < before[key.as_str()]["hits"].as_f64().unwrap_or(0.0)
        })
        .collect();
    let mut metrics = serde_json::Map::new();
    if let (Some(l), Some(r)) = (left["metrics"].as_object(), right["metrics"].as_object()) {
        for (key, value) in l {
            if let Some(other) = r.get(key) {
                let a = value
                    .as_f64()
                    .ok_or_else(|| crate::Error::new("recall snapshot metric is not numeric"))?;
                let b = other
                    .as_f64()
                    .ok_or_else(|| crate::Error::new("recall snapshot metric is not numeric"))?;
                let integer = |value: &Value| {
                    value
                        .as_i64()
                        .map(i128::from)
                        .or_else(|| value.as_u64().map(i128::from))
                };
                let delta = integer(value)
                    .zip(integer(other))
                    .and_then(|(a, b)| {
                        let delta = b - a;
                        i64::try_from(delta)
                            .map(|delta| json!(delta))
                            .ok()
                            .or_else(|| u64::try_from(delta).map(|delta| json!(delta)).ok())
                    })
                    .unwrap_or_else(|| json!(b - a));
                metrics.insert(
                    key.clone(),
                    json!({"left":value,"right":other,"delta":delta}),
                );
            }
        }
    }
    Ok(
        json!({"schema":1,"kind":"oms-graph-recall-compare","comparable":comparable,"reason":if comparable{""}else{"the snapshots cover different fix commits or use different definitions"},"cases":{"left":before.len(),"right":after.len(),"shared":shared.len()},"metrics":metrics,"improved":improved,"regressed":regressed}),
    )
}
