use crate::{Error, Result, util::*};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const SYMBOLS: &[&str] = &["class", "function", "method", "symbol"];
pub const RELATIONS: &[&str] = &[
    "imports",
    "calls",
    "references",
    "tests",
    "uses",
    "depends_on",
    "reads_env",
];
pub struct Graph {
    pub graph: Value,
    pub nodes: BTreeMap<String, Value>,
    pub out: BTreeMap<String, Vec<Value>>,
    pub ins: BTreeMap<String, Vec<Value>>,
}
pub fn words(text: &str) -> Vec<String> {
    let mut expanded = String::new();
    let mut previous = ' ';
    for ch in text.chars() {
        if (previous.is_lowercase() || previous.is_ascii_digit()) && ch.is_uppercase() {
            expanded.push(' ');
        }
        expanded.push(ch);
        previous = ch;
    }
    expanded
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn hits(term: &str, text: &str) -> bool {
    words(text).iter().any(|word| {
        word == term
            || (term.chars().count() >= 4
                && (word.starts_with(term)
                    || (word.chars().count() >= 4 && term.starts_with(word))))
    })
}
fn rounded(value: f64) -> f64 {
    value.round_ties_even()
}
pub fn line(node: &Value) -> u64 {
    n(&node["metadata"], "line")
}
fn end(node: &Value) -> u64 {
    node["metadata"]["end_line"]
        .as_u64()
        .unwrap_or_else(|| line(node))
}

impl Graph {
    pub fn new(graph: Value) -> Result<Self> {
        if !graph["nodes"].is_array() || !graph["edges"].is_array() {
            return Err(Error::new("project graph has an invalid shape"));
        }
        let mut nodes = BTreeMap::new();
        let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut ins: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for node in array(&graph, "nodes") {
            if !node["id"].is_string() {
                return Err(Error::new("project graph node has no id"));
            }
            nodes.insert(s(node, "id").into(), node.clone());
        }
        for edge in array(&graph, "edges") {
            if !edge["source"].is_string() || !edge["target"].is_string() {
                return Err(Error::new("project graph edge has invalid endpoints"));
            }
            out.entry(s(edge, "source").into())
                .or_default()
                .push(edge.clone());
            ins.entry(s(edge, "target").into())
                .or_default()
                .push(edge.clone());
        }
        for map in [&mut out, &mut ins] {
            for edges in map.values_mut() {
                edges.sort_by(|a, b| {
                    (s(a, "source"), s(a, "target"), s(a, "relation")).cmp(&(
                        s(b, "source"),
                        s(b, "target"),
                        s(b, "relation"),
                    ))
                });
            }
        }
        Ok(Self {
            graph,
            nodes,
            out,
            ins,
        })
    }
    pub fn is_test(&self, id: &str) -> bool {
        self.nodes.get(id).is_some_and(|n| {
            s(n, "kind") == "test" || self.nodes.contains_key(&format!("test:{}", s(n, "path")))
        })
    }
    pub fn node(&self, id: &str) -> Result<&Value> {
        self.nodes
            .get(id)
            .ok_or_else(|| Error::new(format!("unknown graph node: {id}")))
    }
    pub fn find(&self, query: &str, kind: &str, limit: i64, include_tests: bool) -> Vec<Value> {
        let needle = query.trim().to_lowercase();
        let normalized = words(query).join(" ");
        let terms = words(query);
        let candidates: Vec<&Value> = self
            .nodes
            .values()
            .filter(|node| {
                (kind.is_empty() || s(node, "kind") == kind)
                    && (include_tests || kind == "test" || !self.is_test(s(node, "id")))
            })
            .collect();
        let weights: BTreeMap<&str, i64> = terms
            .iter()
            .map(|term| {
                let frequency = candidates
                    .iter()
                    .filter(|node| {
                        hits(
                            term,
                            &format!(
                                "{} {} {} {}",
                                s(node, "name"),
                                s(node, "path"),
                                s(&node["metadata"], "qualname"),
                                s(node, "summary")
                            ),
                        )
                    })
                    .count();
                (
                    term.as_str(),
                    rounded(1.0 + ((candidates.len() + 1) as f64 / (frequency + 1) as f64).log2())
                        .clamp(1.0, 8.0) as i64,
                )
            })
            .collect();
        let mut rows = vec![];
        for node in candidates {
            let name = s(node, "name").to_lowercase();
            let path = s(node, "path").to_lowercase();
            let qual = s(&node["metadata"], "qualname").to_lowercase();
            let summary = s(node, "summary").to_lowercase();
            let mut score = if name == needle
                || (!normalized.is_empty() && words(&name).join(" ") == normalized)
            {
                100
            } else if !needle.is_empty()
                && (qual.contains(&needle)
                    || name.contains(&needle)
                    || (!normalized.is_empty()
                        && (words(&qual).join(" ").contains(&normalized)
                            || words(&name).join(" ").contains(&normalized))))
            {
                70
            } else if !needle.is_empty()
                && (path.contains(&needle)
                    || (!normalized.is_empty() && words(&path).join(" ").contains(&normalized)))
            {
                50
            } else if !normalized.is_empty() && words(&summary).join(" ").contains(&normalized) {
                45
            } else {
                terms
                    .iter()
                    .map(|term| {
                        let weight = weights[term.as_str()];
                        (if hits(term, &format!("{name} {qual}")) {
                            8 * weight
                        } else {
                            0
                        })
                        .max(if hits(term, &path) { 3 * weight } else { 0 })
                        .max(if hits(term, &summary) {
                            2 * weight
                        } else {
                            0
                        })
                    })
                    .sum::<i64>()
                    .min(90)
            };
            if s(node, "kind") == "env" && name == needle {
                score = 103;
            }
            if score > 0 && ["class", "function", "method"].contains(&s(node, "kind")) {
                score += 2;
            }
            if score > 0 {
                let mut row = node.clone();
                row["score"] = json!(score);
                rows.push(row);
            }
        }
        rows.sort_by(|a, b| {
            n(b, "score")
                .cmp(&n(a, "score"))
                .then_with(|| s(a, "id").cmp(s(b, "id")))
        });
        rows.truncate(limit.max(0) as usize);
        rows
    }
    pub fn file_api(&self, path: &str, limit: i64) -> Result<Value> {
        let path = path.replace('\\', "/");
        if !safe_path(&path) {
            return Err(Error::new(
                "project api requires a normalized repo-relative path",
            ));
        }
        let matching: Vec<&Value> = self
            .nodes
            .values()
            .filter(|n| s(n, "path") == path)
            .collect();
        if matching.is_empty() {
            return Err(Error::new(format!(
                "path is not indexed by the project graph: {path}"
            )));
        }
        let roots: Vec<&Value> = matching
            .iter()
            .copied()
            .filter(|n| ["file", "test", "document", "config", "module"].contains(&s(n, "kind")))
            .collect();
        let mut symbols = vec![];
        for node in matching
            .iter()
            .copied()
            .filter(|n| SYMBOLS.contains(&s(n, "kind")))
        {
            let qual = if s(&node["metadata"], "qualname").is_empty() {
                s(node, "name")
            } else {
                s(&node["metadata"], "qualname")
            };
            let signature = if s(&node["metadata"], "signature").is_empty() {
                if s(node, "kind") == "class" {
                    format!("class {qual}")
                } else {
                    format!("def {qual}(...)")
                }
            } else {
                s(&node["metadata"], "signature").into()
            };
            let mut row = json!({"id":node["id"],"kind":node["kind"],"name":node["name"],"qualname":qual,"line":line(node),"end_line":end(node),"signature":signature});
            if !s(node, "summary").is_empty() {
                row["summary"] = node["summary"].clone();
            }
            symbols.push(row);
        }
        symbols.sort_by(|a, b| {
            (
                if n(a, "line") == 0 {
                    1 << 31
                } else {
                    n(a, "line")
                },
                s(a, "qualname"),
                s(a, "id"),
            )
                .cmp(&(
                    if n(b, "line") == 0 {
                        1 << 31
                    } else {
                        n(b, "line")
                    },
                    s(b, "qualname"),
                    s(b, "id"),
                ))
        });
        let total = symbols.len();
        symbols.truncate(limit.clamp(1, 2000) as usize);
        Ok(
            json!({"schema":1,"path":path,"language":roots.iter().map(|n|s(n,"language")).find(|s|!s.is_empty()).unwrap_or(""),"content_trust":"untrusted-source-data","summary":roots.iter().map(|n|s(n,"summary")).find(|s|!s.is_empty()),"total_symbols":total,"returned_symbols":symbols.len(),"truncated":symbols.len()<total,"symbols":symbols}),
        )
    }
    pub fn neighbors(&self, id: &str, relation: &str, direction: &str) -> Result<Vec<Value>> {
        self.node(id)?;
        let mut rows = vec![];
        for (map, dir, field) in [(&self.out, "out", "target"), (&self.ins, "in", "source")] {
            if direction != dir && direction != "both" {
                continue;
            }
            for edge in map.get(id).into_iter().flatten() {
                if relation.is_empty() || s(edge, "relation") == relation {
                    rows.push(json!({"id":edge[field],"relation":edge["relation"],"direction":dir,"confidence":edge["confidence"]}));
                }
            }
        }
        rows.sort_by(|a, b| {
            (s(a, "id"), s(a, "relation"), s(a, "direction")).cmp(&(
                s(b, "id"),
                s(b, "relation"),
                s(b, "direction"),
            ))
        });
        Ok(rows)
    }
    pub fn trace(
        &self,
        id: &str,
        direction: &str,
        depth: i64,
        confidences: &[&str],
        max_nodes: usize,
        max_edges: usize,
    ) -> Result<Value> {
        self.node(id)?;
        let mut seen = BTreeMap::from([(id.to_owned(), json!({"id":id,"distance":0,"via":""}))]);
        let mut queue = VecDeque::from([id.to_owned()]);
        let mut edges = vec![];
        let mut omitted = 0;
        let mut limited = false;
        while let Some(current) = queue.pop_front() {
            let distance = n(&seen[&current], "distance");
            if distance as i128 >= depth.max(0) as i128 {
                continue;
            }
            let candidates = if direction == "out" {
                &self.out
            } else {
                &self.ins
            };
            for edge in candidates.get(&current).into_iter().flatten() {
                if !confidences.is_empty() && !confidences.contains(&s(edge, "confidence")) {
                    continue;
                }
                let adjacent = s(
                    edge,
                    if direction == "out" {
                        "target"
                    } else {
                        "source"
                    },
                );
                if !seen.contains_key(adjacent) && seen.len() >= max_nodes.max(1) {
                    limited = true;
                    omitted += 1;
                    continue;
                }
                if !seen.contains_key(adjacent) {
                    seen.insert(adjacent.into(),json!({"id":adjacent,"distance":distance+1,"via":edge["relation"],"confidence":edge["confidence"]}));
                    queue.push_back(adjacent.into());
                }
                if edges.len() >= max_edges {
                    omitted += 1;
                    continue;
                }
                let mut evidence = json!({});
                for key in ["path", "line"] {
                    if let Some(v) = edge["evidence"].get(key) {
                        evidence[key] = v.clone();
                    }
                }
                edges.push(json!({"source":edge["source"],"target":edge["target"],"relation":edge["relation"],"confidence":edge["confidence"],"evidence":evidence}));
            }
        }
        let mut rows: Vec<Value> = seen.into_values().collect();
        rows.sort_by(|a, b| (n(a, "distance"), s(a, "id")).cmp(&(n(b, "distance"), s(b, "id"))));
        edges.sort_by(|a, b| {
            (s(a, "source"), s(a, "target"), s(a, "relation")).cmp(&(
                s(b, "source"),
                s(b, "target"),
                s(b, "relation"),
            ))
        });
        Ok(
            json!({"schema":1,"root":id,"direction":direction,"depth":depth,"nodes":rows,"edges":edges,"limits":{"nodes":max_nodes.max(1),"edges":max_edges},"truncated":limited||omitted>0,"node_limit_reached":limited,"omitted_edges":omitted}),
        )
    }
    pub fn blast(
        &self,
        paths: &[String],
        depth: Option<i64>,
        confidences: &[&str],
        seed_ids: &[String],
    ) -> Result<Value> {
        if depth.is_some_and(|d| d < 0) {
            return Err(Error::new("blast depth must be zero or greater"));
        }
        let mut matched = BTreeSet::new();
        let mut seeds = BTreeSet::new();
        for path in paths {
            for (id, node) in &self.nodes {
                if s(node, "path") == path || (id == path && path.starts_with("env:")) {
                    matched.insert(path.clone());
                    seeds.insert(id.clone());
                }
            }
        }
        for id in seed_ids {
            if let Some(node) = self.nodes.get(id) {
                seeds.insert(id.clone());
                if !s(node, "path").is_empty() {
                    matched.insert(s(node, "path").into());
                }
            }
        }
        let mut seen: BTreeMap<String, Value> = seeds
            .iter()
            .map(|id| (id.clone(), json!({"id":id,"distance":0,"via":""})))
            .collect();
        let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
        let allowed = |edge: &Value| {
            RELATIONS.contains(&s(edge, "relation"))
                && (confidences.is_empty() || confidences.contains(&s(edge, "confidence")))
        };
        while let Some(current) = queue.pop_front() {
            let distance = n(&seen[&current], "distance");
            if depth.is_some_and(|d| distance >= d as u64) {
                continue;
            }
            for edge in self
                .ins
                .get(&current)
                .into_iter()
                .flatten()
                .filter(|e| allowed(e))
            {
                let adjacent = s(edge, "source");
                if !seen.contains_key(adjacent) {
                    seen.insert(adjacent.into(),json!({"id":adjacent,"distance":distance+1,"via":edge["relation"],"confidence":edge["confidence"]}));
                    queue.push_back(adjacent.into());
                }
            }
        }
        let truncated = depth.is_some_and(|d| {
            seen.iter().any(|(id, row)| {
                n(row, "distance") == d as u64
                    && self
                        .ins
                        .get(id)
                        .into_iter()
                        .flatten()
                        .any(|e| allowed(e) && !seen.contains_key(s(e, "source")))
            })
        });
        let mut dependents: Vec<Value> = seen
            .iter()
            .filter(|(id, _)| !seeds.contains(*id))
            .map(|(_, v)| v.clone())
            .collect();
        dependents
            .sort_by(|a, b| (n(a, "distance"), s(a, "id")).cmp(&(n(b, "distance"), s(b, "id"))));
        let files: BTreeSet<&str> = seen
            .keys()
            .filter_map(|id| self.nodes.get(id))
            .map(|n| s(n, "path"))
            .filter(|s| !s.is_empty())
            .collect();
        let tests: Vec<&str> = files
            .iter()
            .copied()
            .filter(|p| self.nodes.contains_key(&format!("test:{p}")))
            .collect();
        let cases:Vec<Value>=seen.keys().filter_map(|id|self.nodes.get(id)).filter(|node|tests.contains(&s(node,"path"))&&["function","method"].contains(&s(node,"kind"))&&s(node,"name").starts_with("test_")).map(|n|json!({"id":n["id"],"language":n["language"],"name":n["name"],"path":n["path"]})).collect();
        let unmatched: Vec<&String> = paths
            .iter()
            .filter(|p| !matched.contains(*p))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(
            json!({"seeds":seeds,"dependents":dependents,"files":files,"tests":tests,"test_cases":cases,"unmatched":unmatched,"path_coverage":if unmatched.is_empty(){"complete"}else{"partial"},"truncated":truncated}),
        )
    }
    pub fn map_summary(&self, include_tests: bool) -> Value {
        let ids: BTreeSet<&str> = self
            .nodes
            .keys()
            .filter(|id| include_tests || !self.is_test(id))
            .map(String::as_str)
            .collect();
        let degrees = super::analytics::degrees(self, &ids);
        let mut kinds = BTreeMap::<&str, usize>::new();
        let mut languages = BTreeMap::<&str, usize>::new();
        let mut areas = BTreeMap::<String, Value>::new();
        let mut groups = BTreeMap::<String, Vec<String>>::new();
        let mut confidence = BTreeMap::<&str, usize>::new();
        for id in &ids {
            let node = &self.nodes[*id];
            *kinds.entry(s(node, "kind")).or_default() += 1;
            let language = s(node, "language");
            let language = if language.is_empty() {
                "unknown"
            } else {
                language
            };
            *languages.entry(language).or_default() += 1;
            let path = s(node, "path");
            if s(node, "kind") == "file" {
                let key = path
                    .split_once('/')
                    .map(|(p, _)| format!("{p}/"))
                    .unwrap_or_else(|| "./".into());
                let area = areas
                    .entry(key)
                    .or_insert(json!({"files":0,"languages":{}}));
                area["files"] = json!(n(area, "files") + 1);
                let count = area["languages"][language].as_u64().unwrap_or(0);
                area["languages"][language] = json!(count + 1);
            }
            if s(node, "kind") == "module" {
                groups
                    .entry(
                        path.split_once('/')
                            .map(|(p, _)| p)
                            .unwrap_or("root")
                            .into(),
                    )
                    .or_default()
                    .push((*id).into());
            }
        }
        for edge in array(&self.graph, "edges") {
            if ids.contains(s(edge, "source")) && ids.contains(s(edge, "target")) {
                *confidence.entry(s(edge, "confidence")).or_default() += 1;
            }
        }
        let mut hubs:Vec<Value>=ids.iter().map(|id|json!({"id":id,"kind":self.nodes[*id]["kind"],"degree":degrees.get(*id).copied().unwrap_or_default().2})).collect();
        hubs.sort_by(|a, b| {
            n(b, "degree")
                .cmp(&n(a, "degree"))
                .then_with(|| s(a, "id").cmp(s(b, "id")))
        });
        let file_hubs: Vec<Value> = hubs
            .iter()
            .filter(|v| s(v, "kind") == "file")
            .take(8)
            .cloned()
            .collect();
        hubs.truncate(10);
        let assurance = super::analytics::assurance(self, include_tests);
        let mut attention: Vec<String> = ids
            .iter()
            .filter(|id| s(&assurance["nodes"][**id], "assurance") == "attention")
            .map(|id| (*id).to_owned())
            .collect();
        attention.sort_by_key(|id| {
            (
                std::cmp::Reverse(degrees.get(id).copied().unwrap_or_default().2),
                id.clone(),
            )
        });
        attention.truncate(10);
        json!({"revision":self.graph["revision"],"counts":{"kind":kinds,"language":languages,"confidence":confidence},"hubs":hubs,"file_hubs":file_hubs,"areas":areas,"groups":groups,"assurance":{"schema":1,"basis":assurance["basis"],"note":assurance["note"],"counts":assurance["counts"],"attention":attention}})
    }
    pub fn unreferenced(&self, limit: i64, include_tests: bool) -> Value {
        let mut rows:Vec<Value>=self.nodes.iter().filter(|(id,node)|SYMBOLS.contains(&s(node,"kind"))&&(include_tests||!self.is_test(id))&&s(node,"name")!="main"&&!(s(node,"name").starts_with("__")&&s(node,"name").ends_with("__"))&&!self.ins.get(*id).into_iter().flatten().any(|e|s(e,"relation")!="contains")).map(|(id,n)|json!({"id":id,"kind":n["kind"],"path":n["path"],"line":n["metadata"]["line"]})).collect();
        rows.sort_by(|a, b| {
            (s(a, "path"), n(a, "line"), s(a, "id")).cmp(&(s(b, "path"), n(b, "line"), s(b, "id")))
        });
        let total = rows.len();
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        json!({"note":"candidates only: dynamic calls, decorators, entrypoint tables, framework hooks and unparsed files are invisible to the graph, so none of these is a confirmed dead symbol","total":total,"truncated":rows.len()<total,"candidates":rows})
    }
    pub fn visual(
        &self,
        limit: i64,
        depth: i64,
        include_tests: bool,
        coverage: &Value,
        focus: &[String],
        paths: &[String],
        changed: &[String],
        impacted: &[String],
    ) -> Value {
        super::visual::model(
            self,
            limit,
            depth,
            include_tests,
            coverage,
            focus,
            paths,
            changed,
            impacted,
        )
    }
    pub fn search(
        &self,
        repo: &Path,
        query: &str,
        limit: i64,
        ranked: bool,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        if query.is_empty() || query.len() > 1024 || ranked && query.trim().is_empty() {
            return Err(Error::new(if ranked {
                "project search --ranked requires a non-empty query of at most 1024 bytes"
            } else {
                "project search requires a non-empty query of at most 1024 bytes"
            }));
        }
        let mut terms = vec![];
        for word in words(query) {
            if !terms.contains(&word) && terms.len() < 16 {
                terms.push(word);
            }
        }
        if ranked && terms.is_empty() {
            return Err(Error::new(
                "project search --ranked found no word in the query",
            ));
        }
        let roots: BTreeMap<&str, &Value> = self
            .nodes
            .values()
            .filter(|node| {
                ["file", "test", "document", "config"].contains(&s(node, "kind"))
                    && !s(node, "path").is_empty()
            })
            .map(|node| (s(node, "path"), node))
            .collect();
        let root =
            oms_safe_fs::Root::open(repo).map_err(|e| Error::new(format!("source root: {e:?}")))?;
        let mut searched = 0;
        let mut stale = 0;
        let mut skipped = 0;
        let mut total = 0;
        let mut groups: BTreeMap<String, Value> = BTreeMap::new();
        let mut blocks: BTreeMap<
            String,
            (Value, usize, BTreeMap<String, usize>, usize, u64, String),
        > = BTreeMap::new();
        let cap = (limit as usize * 20).max(1000);
        let mut captured = 0;
        for (path, node) in &roots {
            check(cancel)?;
            if s(node, "language") == "notebook" {
                skipped += 1;
                continue;
            }
            let data = match root.read(path, 2 * 1024 * 1024, oms_safe_fs::Policy::source()) {
                Ok(v) => v,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if sha(&data) != s(node, "source_digest") {
                stale += 1;
                continue;
            }
            searched += 1;
            let symbols: Vec<&Value> = self
                .nodes
                .values()
                .filter(|n| s(n, "path") == *path && SYMBOLS.contains(&s(n, "kind")))
                .collect();
            for (offset, text) in String::from_utf8_lossy(&data).lines().enumerate() {
                let number = offset as u64 + 1;
                let owner = symbols
                    .iter()
                    .copied()
                    .filter(|n| line(n) <= number && (end(n) == 0 || number <= end(n)))
                    .max_by_key(|n| (line(n), std::cmp::Reverse(end(n)), s(n, "id")))
                    .unwrap_or(node);
                if ranked {
                    let block = blocks.entry(s(owner, "id").into()).or_insert((
                        owner.clone(),
                        0,
                        BTreeMap::new(),
                        0,
                        0,
                        String::new(),
                    ));
                    let words = words(text);
                    block.1 += words.len();
                    let mut distinct = BTreeSet::new();
                    for word in words {
                        if terms.contains(&word) {
                            *block.2.entry(word.clone()).or_default() += 1;
                            distinct.insert(word);
                        }
                    }
                    if distinct.len() > block.3 {
                        block.3 = distinct.len();
                        block.4 = number;
                        block.5 = text.into();
                    }
                } else if text.to_lowercase().contains(&query.to_lowercase()) {
                    total += 1;
                    if captured >= cap {
                        continue;
                    }
                    captured += 1;
                    let group=groups.entry(s(owner,"id").into()).or_insert_with(||json!({"id":owner["id"],"kind":owner["kind"],"path":owner["path"],"name":owner["name"],"incoming":self.ins.get(s(owner,"id")).map_or(0,Vec::len),"hits":[]}));
                    let rows = group["hits"].as_array_mut().unwrap();
                    if rows.len() < 8 {
                        rows.push(json!({"line":number,"text":scrub(&text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect::<String>())}));
                    }
                }
            }
        }
        if ranked {
            let count = blocks.len();
            let average =
                blocks.values().map(|b| b.1).sum::<usize>() as f64 / (count.max(1) as f64);
            let mut rows = vec![];
            for (node, length, counts, _, preview_line, preview) in blocks.values() {
                if counts.is_empty() {
                    continue;
                }
                let mut score = 0.0;
                for term in &terms {
                    let frequency = *counts.get(term).unwrap_or(&0) as f64;
                    if frequency == 0.0 {
                        continue;
                    }
                    let docs = blocks.values().filter(|b| b.2.contains_key(term)).count() as f64;
                    let idf = (1.0 + (count as f64 - docs + 0.5) / (docs + 0.5)).ln();
                    score += idf * frequency * 2.2
                        / (frequency + 1.2 * (0.25 + 0.75 * (*length as f64 / average.max(1.0))));
                }
                let matched: Vec<&String> =
                    terms.iter().filter(|t| counts.contains_key(*t)).collect();
                rows.push(json!({"id":node["id"],"kind":node["kind"],"path":node["path"],"line":line(node),"score":rounded(score*10000.0)/10000.0,"matched_terms":matched,"preview":{"line":preview_line,"text":scrub(&preview.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect::<String>())}}));
            }
            rows.sort_by(|a, b| {
                b["score"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&a["score"].as_f64().unwrap_or(0.0))
                    .then_with(|| {
                        array(b, "matched_terms")
                            .len()
                            .cmp(&array(a, "matched_terms").len())
                    })
                    .then_with(|| {
                        (s(a, "path"), n(a, "line"), s(a, "id")).cmp(&(
                            s(b, "path"),
                            n(b, "line"),
                            s(b, "id"),
                        ))
                    })
            });
            let total = rows.len();
            rows.truncate(limit.clamp(1, 500) as usize);
            return Ok(
                json!({"schema":1,"kind":"oms-project-ranked-search","query":query,"terms":terms,"match":"bm25-symbol-blocks","content_trust":"untrusted-source-data","indexed_files":roots.len(),"searched_files":searched,"stale_files":stale,"skipped_files":skipped,"blocks":count,"total_matches":total,"returned":rows.len(),"truncated":rows.len()<total,"results":rows}),
            );
        }
        let mut ordered: Vec<Value> = groups.into_values().collect();
        ordered.sort_by(|a, b| {
            n(b, "incoming").cmp(&n(a, "incoming")).then_with(|| {
                (s(a, "path"), n(&a["hits"][0], "line"), s(a, "id")).cmp(&(
                    s(b, "path"),
                    n(&b["hits"][0], "line"),
                    s(b, "id"),
                ))
            })
        });
        let mut returned = 0;
        let mut rows = vec![];
        for mut group in ordered {
            if returned >= limit as usize {
                break;
            }
            group["hits"]
                .as_array_mut()
                .unwrap()
                .truncate(limit as usize - returned);
            returned += array(&group, "hits").len();
            rows.push(group);
        }
        Ok(
            json!({"schema":1,"query":query,"match":"literal-case-insensitive","content_trust":"untrusted-source-data","indexed_files":roots.len(),"searched_files":searched,"stale_files":stale,"skipped_files":skipped,"total_hits":total,"returned_hits":returned,"truncated":returned<total,"groups":rows}),
        )
    }
    pub fn symbol(
        &self,
        repo: &Path,
        name: &str,
        limit: i64,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        let name = name.trim();
        if name.is_empty() || name.len() > 512 {
            return Err(Error::new(
                "project symbol requires a non-empty name of at most 512 bytes",
            ));
        }
        let mut found = vec![];
        let mut mode = "none";
        if let Some(node) = self
            .nodes
            .get(name)
            .filter(|n| SYMBOLS.contains(&s(n, "kind")))
        {
            found.push(node);
            mode = "id";
        } else {
            for (label, fold) in [("exact", false), ("case-insensitive", true)] {
                found = self
                    .nodes
                    .values()
                    .filter(|n| {
                        SYMBOLS.contains(&s(n, "kind"))
                            && [s(n, "name"), s(&n["metadata"], "qualname")]
                                .iter()
                                .any(|s| {
                                    if fold {
                                        s.to_lowercase() == name.to_lowercase()
                                    } else {
                                        *s == name
                                    }
                                })
                    })
                    .collect();
                if !found.is_empty() {
                    mode = label;
                    break;
                }
            }
        }
        found.sort_by_key(|n| (s(n, "path"), s(n, "id")));
        let mut omitted = json!({"definitions":found.len().saturating_sub(20)});
        found.truncate(20);
        let mut definitions = vec![];
        let mut callers = vec![];
        let mut tests = vec![];
        let mut inferred = vec![];
        let mut linked = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for node in &found {
            let api = self.file_api(s(node, "path"), 2000)?;
            if let Some(symbol) = array(&api, "symbols")
                .iter()
                .find(|r| r["id"] == node["id"])
            {
                let mut row = symbol.clone();
                row.as_object_mut().unwrap().remove("name");
                row.as_object_mut().unwrap().remove("summary");
                row["path"] = node["path"].clone();
                definitions.push(row);
            }
            for edge in self.ins.get(s(node, "id")).into_iter().flatten() {
                if ["contains", "defines"].contains(&s(edge, "relation")) {
                    continue;
                }
                let key = (
                    s(edge, "source"),
                    s(node, "id"),
                    s(edge, "relation"),
                    s(edge, "confidence"),
                    n(&edge["evidence"], "line"),
                );
                if !seen.insert(key) {
                    continue;
                }
                let path = s(&edge["evidence"], "path");
                let line = n(&edge["evidence"], "line");
                linked.insert((path.to_owned(), line));
                let row = json!({"source":edge["source"],"target":node["id"],"relation":edge["relation"],"confidence":edge["confidence"],"path":path,"line":line});
                if s(edge, "confidence") != "EXTRACTED" {
                    inferred.push(row);
                } else if self.is_test(s(edge, "source")) {
                    tests.push(row);
                } else {
                    callers.push(row);
                }
            }
        }
        for (key, rows) in [
            ("callers", &mut callers),
            ("tests", &mut tests),
            ("inferred", &mut inferred),
        ] {
            rows.sort_by(|a, b| {
                (
                    s(a, "path"),
                    n(a, "line"),
                    s(a, "source"),
                    s(a, "relation"),
                    s(a, "target"),
                )
                    .cmp(&(
                        s(b, "path"),
                        n(b, "line"),
                        s(b, "source"),
                        s(b, "relation"),
                        s(b, "target"),
                    ))
            });
            omitted[key] = json!(rows.len().saturating_sub(limit as usize));
            rows.truncate(limit as usize);
        }
        let bare = found
            .first()
            .map(|n| {
                if s(&n["metadata"], "qualname").is_empty() {
                    s(n, "name")
                } else {
                    s(&n["metadata"], "qualname")
                }
            })
            .unwrap_or(name)
            .rsplit('.')
            .next()
            .unwrap_or(name);
        let scan = self.search(repo, bare, 500, false, cancel)?;
        let pattern = regex::Regex::new(&format!(
            r"(?i)(?:^|[^A-Za-z0-9_]){}(?:$|[^A-Za-z0-9_])",
            regex::escape(bare)
        ))
        .map_err(|e| Error::new(e.to_string()))?;
        let mut text_only = vec![];
        for group in array(&scan, "groups") {
            for hit in array(group, "hits") {
                let path = s(group, "path");
                let number = n(hit, "line");
                if linked.contains(&(path.into(), number))
                    || found.iter().any(|node| {
                        s(node, "path") == path && line(node) <= number && number <= end(node)
                    })
                {
                    continue;
                }
                if s(hit, "text") != "[sensitive-content-omitted]"
                    && !pattern.is_match(s(hit, "text"))
                {
                    continue;
                }
                text_only.push(
                    json!({"path":path,"line":number,"within":group["id"],"text":hit["text"]}),
                );
            }
        }
        text_only.sort_by(|a, b| (s(a, "path"), n(a, "line")).cmp(&(s(b, "path"), n(b, "line"))));
        omitted["text_only"] = json!(text_only.len().saturating_sub(limit as usize));
        text_only.truncate(limit as usize);
        Ok(
            json!({"schema":1,"kind":"oms-project-symbol","query":name,"match":mode,"text_query":bare,"content_trust":"untrusted-source-data","extracted":{"definitions":definitions,"callers":callers,"tests":tests},"inferred":inferred,"text_only":text_only,"omitted":omitted,"text_scan":{"searched_files":scan["searched_files"],"stale_files":scan["stale_files"],"skipped_files":scan["skipped_files"],"truncated":scan["truncated"]}}),
        )
    }
}
fn counts_block(title: &str, value: &Value) -> Vec<String> {
    let mut rows = value
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return vec![format!("{title}: (none)")];
    }
    rows.sort_by_key(|(k, n)| (std::cmp::Reverse(*n), k.clone()));
    let width = rows
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!("{title}:")];
    lines.extend(
        rows.into_iter()
            .map(|(name, count)| format!("  {name:width$}  {count}")),
    );
    lines
}
pub fn map_text(value: &Value) -> String {
    let revision = s(value, "revision");
    let mut lines = vec![format!(
        "project graph (revision {})",
        if revision.is_empty() {
            "unknown"
        } else {
            revision
        }
    )];
    for (label, key) in [
        ("nodes by kind", "kind"),
        ("nodes by language", "language"),
        ("edges by confidence", "confidence"),
    ] {
        lines.extend(counts_block(label, &value["counts"][key]));
    }
    lines.extend(counts_block(
        "structural assurance",
        &value["assurance"]["counts"],
    ));
    let coverage = &value["coverage"];
    lines.push(format!(
        "parser coverage: parsed {}, unparsed {}",
        n(coverage, "parsed"),
        n(coverage, "unparsed")
    ));
    if let Some(extensions) = coverage["unparsed_by_extension"]
        .as_object()
        .filter(|o| !o.is_empty())
    {
        let mut rows = extensions.iter().collect::<Vec<_>>();
        rows.sort_by_key(|(name, _)| *name);
        lines.push(format!(
            "  unparsed extensions: {}",
            rows.into_iter()
                .map(|(name, count)| format!("{name}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let hubs = array(value, "hubs");
    lines.push(
        if hubs.is_empty() {
            "top hubs: (none)"
        } else {
            "top hubs:"
        }
        .into(),
    );
    for (i, row) in hubs.iter().enumerate() {
        lines.push(format!(
            "  {:2}. {}  ({})  degree {}",
            i + 1,
            s(row, "id"),
            s(row, "kind"),
            n(row, "degree")
        ));
    }
    let mut groups = value["groups"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_array().cloned().unwrap_or_default()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    groups.sort_by_key(|(name, members)| (std::cmp::Reverse(members.len()), name.clone()));
    lines.push(
        if groups.is_empty() {
            "module groups: (none)"
        } else {
            "module groups:"
        }
        .into(),
    );
    for (name, members) in groups {
        let mut shown = members
            .iter()
            .take(5)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if members.len() > 5 {
            shown.push_str(&format!(", +{} more", members.len() - 5));
        }
        lines.push(format!("  {name} ({}): {shown}", members.len()));
    }
    lines.join("\n") + "\n"
}
pub fn symbol_text(value: &Value) -> String {
    let mut out = format!(
        "symbol {}: {} definition(s) ({} match); orientation, not proof of use or disuse\n",
        s(value, "query"),
        array(&value["extracted"], "definitions").len(),
        s(value, "match")
    );
    for key in ["definitions", "callers", "tests"] {
        out.push_str(&format!("extracted {key}:\n"));
        for row in array(&value["extracted"], key) {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                s(row, "path"),
                n(row, "line"),
                if key == "definitions" {
                    s(row, "signature")
                } else {
                    s(row, "source")
                }
            ));
        }
    }
    for key in ["inferred", "text_only"] {
        out.push_str(&format!("{key}:\n"));
        for row in array(value, key) {
            out.push_str(&format!(
                "  {}:{}  {}\n",
                s(row, "path"),
                n(row, "line"),
                s(row, "text")
            ));
        }
    }
    out
}
pub fn orientation(graph: &Graph, manifest: &Value, name: &str) -> String {
    let summary = graph.map_summary(false);
    let plain = |s: &str| {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut lines = vec![format!(
        "[oms graph] {} revision {} · built {} UTC · {} files, {} nodes",
        plain(name),
        s(&summary, "revision").chars().take(12).collect::<String>(),
        if s(manifest, "generated_at").is_empty() {
            "unknown".into()
        } else {
            s(manifest, "generated_at")
                .chars()
                .take(16)
                .collect::<String>()
                .replace('T', " ")
        },
        n(&manifest["stats"], "files"),
        n(&manifest["stats"], "nodes")
    )];
    let mut areas = summary["areas"]
        .as_object()
        .map(|o| o.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    areas.sort_by_key(|(name, row)| (std::cmp::Reverse(n(row, "files")), *name));
    if !areas.is_empty() {
        let mut parts = vec![];
        for (name, row) in areas.iter().take(3) {
            let mut languages = row["languages"]
                .as_object()
                .map(|o| o.iter().collect::<Vec<_>>())
                .unwrap_or_default();
            languages.sort_by_key(|(name, count)| {
                (std::cmp::Reverse(count.as_u64().unwrap_or(0)), *name)
            });
            let mix = if languages.len() > 1 {
                format!(
                    " ({})",
                    languages
                        .iter()
                        .take(2)
                        .map(|(name, count)| format!("{name} {count}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            };
            parts.push(format!("{} {}{mix}", plain(name), n(row, "files")));
        }
        if areas.len() > 3 {
            parts.push(format!("+{}", areas.len() - 3));
        }
        lines.push(format!("areas: {}", parts.join(" · ")));
    }
    let hubs = array(&summary, "file_hubs")
        .iter()
        .take(6)
        .map(|row| {
            format!(
                "{} {}",
                plain(
                    s(row, "id")
                        .split_once(':')
                        .map(|(_, p)| p)
                        .unwrap_or(s(row, "id"))
                ),
                n(row, "degree")
            )
        })
        .collect::<Vec<_>>();
    if !hubs.is_empty() {
        lines.push(format!("file hubs: {}", hubs.join(" · ")));
    }
    let counts = &summary["assurance"]["counts"];
    let need = n(counts, "needs-evidence") + n(counts, "attention");
    if need > 0 {
        lines.push(format!(
            "attention: {need} nodes need structural evidence (oms graph project map)"
        ));
    }
    lines.push("ask the graph: oms graph project find NAME · api PATH · blast --path PATH · context --task \"...\"".into());
    lines.push(
        "(repeats only when the graph revision changes; the graph refreshes in the background)"
            .into(),
    );
    let mut kept = vec![];
    let mut size = 0;
    for line in lines {
        let added = line.len() + usize::from(!kept.is_empty());
        if size + added > 1536 {
            break;
        }
        size += added;
        kept.push(line);
    }
    kept.join("\n")
}
pub fn html(value: &Value) -> String {
    super::visual::html(value, "OMS Project Graph")
}
