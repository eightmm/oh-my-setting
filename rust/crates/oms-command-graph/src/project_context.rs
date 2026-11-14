use super::{
    analytics,
    index::{Graph, words},
};
use crate::{Error, Result, util::*};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn measure(pack: &mut Value) -> Result<()> {
    loop {
        let size = bytes(pack, false)?.len() - 1;
        if n(pack, "pack_bytes") == size as u64 {
            return Ok(());
        }
        pack["pack_bytes"] = json!(size);
    }
}
fn rank(repo: &Path, index: &Graph, terms: &[String], cancel: &AtomicBool) -> Result<Vec<Value>> {
    let mut unique = vec![];
    for t in terms {
        if !unique.contains(t) {
            unique.push(t.clone());
        }
    }
    if unique.is_empty() {
        return Ok(vec![]);
    }
    let root =
        oms_safe_fs::Root::open(repo).map_err(|e| Error::new(format!("source root: {e:?}")))?;
    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let candidates: Vec<&Value> = index
        .nodes
        .values()
        .filter(|n| !index.is_test(s(n, "id")))
        .collect();
    for node in &candidates {
        check(cancel)?;
        let path = s(node, "path");
        if path.is_empty() || sources.contains_key(path) {
            continue;
        }
        let mut lines = vec![];
        if !path.to_lowercase().ends_with(".ipynb") {
            if let Ok(data) = root.read(path, 2 * 1024 * 1024, oms_safe_fs::Policy::source()) {
                if sha(&data) == s(node, "source_digest") {
                    lines = String::from_utf8_lossy(&data)
                        .lines()
                        .map(str::to_owned)
                        .collect();
                }
            }
        }
        sources.insert(path.into(), lines);
    }
    let quoted =
        regex::Regex::new(r#"'([^'\\]*(?:\\.[^'\\]*)*)'|"([^"\\]*(?:\\.[^"\\]*)*)""#).unwrap();
    let mut docs = vec![];
    for node in candidates {
        let path = s(node, "path");
        let lines = sources.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let start = n(&node["metadata"], "line") as usize;
        let end = n(&node["metadata"], "end_line") as usize;
        let body = if start > 0 {
            lines
                .get(start - 1..end.min(lines.len()))
                .unwrap_or(&[])
                .join("\n")
        } else {
            lines.join("\n")
        };
        let quoted = quoted
            .captures_iter(&body)
            .map(|c| {
                c.get(1)
                    .or_else(|| c.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .join(" ");
        let fields = [
            format!("{} {}", s(node, "name"), s(&node["metadata"], "qualname")),
            path.into(),
            s(node, "summary").into(),
            quoted,
        ];
        let counters: Vec<BTreeMap<String, usize>> = fields
            .iter()
            .map(|field| {
                let mut counts = BTreeMap::new();
                for term in words(field) {
                    *counts.entry(term).or_default() += 1;
                }
                counts
            })
            .collect();
        let lengths: Vec<usize> = counters.iter().map(|c| c.values().sum()).collect();
        docs.push((node, counters, lengths));
    }
    let count = docs.len();
    let averages: Vec<f64> = (0..4)
        .map(|f| docs.iter().map(|d| d.2[f]).sum::<usize>() as f64 / count.max(1) as f64)
        .collect();
    let mut rows = vec![];
    for (node, counters, lengths) in &docs {
        let mut score = 0.0;
        for term in &unique {
            let frequency = docs
                .iter()
                .filter(|d| d.1.iter().any(|c| c.contains_key(term)))
                .count() as f64;
            let weight = (1.0 + (count as f64 - frequency + 0.5) / (frequency + 0.5)).ln();
            for field in 0..4 {
                let tf = *counters[field].get(term).unwrap_or(&0) as f64;
                if tf > 0.0 {
                    let norm =
                        1.2 * (0.25 + 0.75 * lengths[field] as f64 / averages[field].max(1.0));
                    score += [3.0, 1.0, 1.5, 0.6][field] * weight * tf * 2.2 / (tf + norm);
                }
            }
        }
        if score > 0.0 {
            rows.push(json!({"id":node["id"],"score":(score*100.0).round_ties_even()as i64}));
        }
    }
    rows.sort_by(|a, b| {
        n(b, "score").cmp(&n(a, "score")).then_with(|| {
            (!s(a, "id").starts_with("symbol:"), s(a, "id"))
                .cmp(&(!s(b, "id").starts_with("symbol:"), s(b, "id")))
        })
    });
    Ok(rows)
}
pub fn select(
    repo: &Path,
    index: &Graph,
    task: &str,
    max_files: i64,
    entry_paths: &[String],
    base: &str,
    coverage: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    if entry_paths.iter().any(|p| !safe_path(p)) {
        return Err(Error::new(
            "entry paths must be normalized repo-relative file paths",
        ));
    }
    let stop = [
        "the", "and", "for", "with", "from", "that", "this", "into", "add", "change", "fix",
        "make", "modify", "update", "use", "are", "was", "will", "please",
    ];
    let terms: Vec<String> = task
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !stop.contains(&w.to_lowercase().as_str()))
        .flat_map(words)
        .filter(|w| w.chars().count() > 1 || !w.is_ascii())
        .collect();
    let raw = if entry_paths.is_empty() {
        rank(repo, index, &terms, cancel)?
    } else {
        entry_paths
            .iter()
            .flat_map(|p| [format!("file:{p}"), format!("test:{p}")])
            .filter(|id| index.nodes.contains_key(id))
            .map(|id| json!({"id":id,"score":100}))
            .collect()
    };
    let mut entries = vec![];
    let mut keys = BTreeSet::new();
    let entry_limit = (if entry_paths.is_empty() {
        8
    } else {
        entry_paths.len()
    })
    .min(max_files.max(0) as usize);
    for row in raw {
        if entries.len() >= entry_limit {
            break;
        }
        let path = s(&index.nodes[s(&row, "id")], "path");
        let key = if path.is_empty() { s(&row, "id") } else { path };
        if keys.insert(key.to_owned()) {
            entries.push(row);
        }
    }
    let mut scores: BTreeMap<String, i64> = BTreeMap::new();
    let mut reasons = BTreeMap::new();
    let mut selected = BTreeSet::new();
    let mut entry_files = vec![];
    for row in &entries {
        let id = s(row, "id");
        selected.insert(id.to_owned());
        let path = s(&index.nodes[id], "path");
        if !path.is_empty() {
            *scores.entry(path.into()).or_default() += 1000 + n(row, "score") as i64;
            reasons.insert(path.to_owned(), format!("query:{id}"));
            if !entry_files.iter().any(|p| p == path) {
                entry_files.push(path.to_owned());
            }
        }
    }
    let change_paths = if base.is_empty() {
        vec![]
    } else {
        super::affected::changed_paths(repo, base, cancel)?
    };
    for path in &change_paths {
        if index.nodes.values().any(|n| s(n, "path") == path) {
            *scores.entry(path.clone()).or_default() += 800;
            reasons
                .entry(path.clone())
                .or_insert(format!("changed:{base}"));
            if !entry_files.contains(path) {
                entry_files.push(path.clone());
            }
        }
    }
    let mut neighbors: BTreeMap<String, (u8, u64, usize, String)> = BTreeMap::new();
    for (rank, entry) in entries.iter().enumerate() {
        for direction in ["out", "in"] {
            let trace = index.trace(
                s(entry, "id"),
                direction,
                2,
                &["EXTRACTED", "INFERRED"],
                128,
                160,
            )?;
            for row in array(&trace, "nodes") {
                if n(row, "distance") == 0 {
                    continue;
                }
                let candidate = (
                    if s(row, "confidence") == "EXTRACTED" {
                        0
                    } else {
                        1
                    },
                    n(row, "distance"),
                    rank,
                    s(entry, "id").to_owned(),
                );
                let id = s(row, "id");
                if neighbors.get(id).is_none_or(|old| candidate < *old) {
                    neighbors.insert(id.into(), candidate);
                }
            }
        }
    }
    let mut neighbors: Vec<_> = neighbors.into_iter().collect();
    neighbors.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
    let mut neighbor_hits = BTreeMap::<String, usize>::new();
    for (id, (_, distance, _, entry)) in neighbors {
        if selected.len() >= 40 {
            break;
        }
        selected.insert(id.clone());
        let path = index.nodes.get(&id).map(|n| s(n, "path")).unwrap_or("");
        let count = neighbor_hits.entry(path.into()).or_default();
        if path.is_empty() || entry_files.iter().any(|p| p == path) || *count >= 3 {
            continue;
        }
        *count += 1;
        *scores.entry(path.into()).or_default() += (40 - distance as i64 * 10).max(10);
        reasons
            .entry(path.into())
            .or_insert(format!("neighbor:{entry}"));
    }
    let blast = index.blast(&entry_files, Some(2), &["EXTRACTED", "INFERRED"], &[])?;
    let mut blast_paths = BTreeMap::<String, u64>::new();
    let mut quality = BTreeMap::<String, (u8, u64)>::new();
    for row in array(&blast, "dependents") {
        let id = s(row, "id");
        let path = index.nodes.get(id).map(|n| s(n, "path")).unwrap_or("");
        if path.is_empty() {
            continue;
        }
        let distance = n(row, "distance");
        blast_paths
            .entry(path.into())
            .and_modify(|old| *old = (*old).min(distance))
            .or_insert(distance);
        if index.is_test(id) {
            let q = (
                if s(row, "confidence") == "EXTRACTED" {
                    0
                } else {
                    1
                },
                distance,
            );
            quality
                .entry(path.into())
                .and_modify(|old| *old = (*old).min(q))
                .or_insert(q);
        }
    }
    let mut blast_paths: Vec<_> = blast_paths.into_iter().collect();
    blast_paths.sort_by_key(|(p, d)| (*d, p.clone()));
    for (path, distance) in blast_paths.into_iter().take(40) {
        if !entry_files.contains(&path) {
            *scores.entry(path.clone()).or_default() += (25 - distance as i64 * 5).max(5);
            reasons.entry(path).or_insert(format!("blast:{distance}"));
        }
    }
    let is_test = |p: &str| index.nodes.contains_key(&format!("test:{p}"));
    let mut files: Vec<String> = scores.keys().cloned().collect();
    files.sort_by_key(|p| {
        (
            is_test(p) && !entry_files.contains(p),
            std::cmp::Reverse(scores[p]),
            p.clone(),
        )
    });
    files.truncate(max_files.max(0) as usize);
    let mut tests: BTreeSet<String> = array(&blast, "tests")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    tests.extend(scores.keys().filter(|p| is_test(p)).cloned());
    let mut tests: Vec<String> = tests.into_iter().collect();
    tests.sort_by_key(|p| {
        (
            quality.get(p).copied().unwrap_or((4, 3)),
            std::cmp::Reverse(scores.get(p).copied().unwrap_or(0)),
            p.clone(),
        )
    });
    tests.truncate(max_files.max(0) as usize);
    let mut cases: Vec<Value> = array(&blast, "test_cases")
        .iter()
        .filter(|r| tests.iter().any(|p| p == s(r, "path")))
        .cloned()
        .collect();
    cases.sort_by_key(|r| {
        (
            std::cmp::Reverse(
                terms
                    .iter()
                    .filter(|t| {
                        !matches!(t.as_str(), "test" | "tests")
                            && s(r, "name").to_lowercase().contains(t.as_str())
                    })
                    .count(),
            ),
            tests
                .iter()
                .position(|p| p == s(r, "path"))
                .unwrap_or(usize::MAX),
            s(r, "id").to_owned(),
        )
    });
    let mut counts = BTreeMap::<String, usize>::new();
    cases.retain(|r| {
        let count = counts.entry(s(r, "path").into()).or_default();
        *count += 1;
        *count <= 4
    });
    cases.truncate((max_files.max(0) * 4).min(40) as usize);
    let degrees = analytics::degrees(index, &index.nodes.keys().map(String::as_str).collect());
    let mut hubs: Vec<Value> = selected
        .iter()
        .filter_map(|id| degrees.get(id).map(|d| json!({"id":id,"degree":d.2})))
        .collect();
    hubs.sort_by(|a, b| {
        n(b, "degree")
            .cmp(&n(a, "degree"))
            .then_with(|| s(a, "id").cmp(s(b, "id")))
    });
    hubs.truncate(10);
    let all = analytics::assurance(index, false);
    let mut assurance_ids = selected.clone();
    for (id, node) in &index.nodes {
        if files.iter().any(|p| p == s(node, "path"))
            && ["file", "module"].contains(&s(node, "kind"))
        {
            assurance_ids.insert(id.clone());
        }
    }
    let mut assurance_ids: Vec<_> = assurance_ids
        .into_iter()
        .filter(|id| all["nodes"].get(id).is_some())
        .collect();
    assurance_ids.sort_by_key(|id| {
        (
            entries
                .iter()
                .position(|e| s(e, "id") == id)
                .unwrap_or(usize::MAX),
            files
                .iter()
                .position(|p| p == s(&index.nodes[id], "path"))
                .unwrap_or(usize::MAX),
            id.clone(),
        )
    });
    assurance_ids.truncate(40);
    let mut assurance_counts = BTreeMap::<String, usize>::new();
    let assurance_rows: Vec<Value> = assurance_ids
        .iter()
        .map(|id| {
            let mut row = all["nodes"][id].clone();
            row["id"] = json!(id);
            row["path"] = index.nodes[id]["path"].clone();
            row["kind"] = index.nodes[id]["kind"].clone();
            *assurance_counts
                .entry(s(&row, "assurance").into())
                .or_default() += 1;
            row
        })
        .collect();
    let change_blast = index.blast(&change_paths, Some(2), &["EXTRACTED"], &[])?;
    let impacted: BTreeSet<String> = array(&change_blast, "dependents")
        .iter()
        .filter_map(|r| index.nodes.get(s(r, "id")))
        .map(|n| s(n, "path"))
        .filter(|p| {
            !p.is_empty()
                && !change_paths.iter().any(|c| c == p)
                && !array(&change_blast, "tests")
                    .iter()
                    .any(|t| t.as_str() == Some(p))
        })
        .map(str::to_owned)
        .collect();
    let evidence: Vec<Value> = files
        .iter()
        .map(|p| json!({"path":p,"reason":reasons[p],"score":scores[p]}))
        .collect();
    let unmatched: Vec<&String> = entry_paths
        .iter()
        .filter(|p| {
            !index.nodes.contains_key(&format!("file:{p}"))
                && !index.nodes.contains_key(&format!("test:{p}"))
        })
        .collect();
    let status = if entries.is_empty() {
        "no-match"
    } else if unmatched.is_empty() {
        "matched"
    } else {
        "partial"
    };
    let mut pack = json!({"task":task,"project_graph_revision":index.graph["revision"],"entries":entries.iter().map(|r|s(r,"id")).collect::<Vec<_>>(),"files":files,"tests":tests,"test_cases":cases,"blast":{"seeds":array(&blast,"seeds").iter().take(40).collect::<Vec<_>>(),"dependents":array(&blast,"dependents").iter().take(40).collect::<Vec<_>>(),"files":array(&blast,"files").iter().take(max_files.max(0)as usize).collect::<Vec<_>>(),"tests":tests,"test_cases":cases,"unmatched":blast["unmatched"],"path_coverage":blast["path_coverage"],"truncated":blast["truncated"],"counts":{"seeds":array(&blast,"seeds").len(),"dependents":array(&blast,"dependents").len(),"files":array(&blast,"files").len(),"tests":array(&blast,"tests").len(),"test_cases":array(&blast,"test_cases").len()}},"hubs":hubs,"evidence":evidence,"assurance":{"schema":1,"basis":all["basis"],"note":all["note"],"counts":assurance_counts,"nodes":assurance_rows},"change":{"schema":1,"enabled":!base.is_empty(),"base":base,"impact_basis":"EXTRACTED reverse dependencies","changed":change_paths,"untracked":[],"impacted":impacted,"unmatched":change_blast["unmatched"],"truncated":change_blast["truncated"]},"coverage":coverage,"pack_bytes":0,"retrieval":{"method":if entry_paths.is_empty(){"lexical"}else{"entry-path"},"status":status,"unmatched_entry_paths":unmatched,"hint":if status=="matched"{""}else{"No complete match. Search source identifiers/paths directly, then use --entry-path. Lexical search does not translate the task; missing matches do not prove missing code."}}});
    measure(&mut pack)?;
    Ok(pack)
}
pub fn persist(state: &Path, repo: &Path, pack: &mut Value) -> Result<()> {
    let raw = bytes(pack, false)?;
    let digest = sha(&raw[..raw.len() - 1]);
    let path = state.join("context").join(format!("{digest}.json"));
    pack["pack_digest"] = json!(digest);
    pack["pack_path"] = json!(
        path.strip_prefix(repo)
            .ok()
            .and_then(Path::to_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("context/{digest}.json"))
    );
    measure(pack)?;
    write_json(&path, pack)
}
pub fn signature_map(
    repo: &Path,
    index: &Graph,
    task: &str,
    max_files: i64,
    max_bytes: i64,
    entry_paths: &[String],
    coverage: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    if !(256..=262144).contains(&max_bytes) {
        return Err(Error::new(
            "signature map --max-bytes must be between 256 and 262144",
        ));
    }
    let pack = select(
        repo,
        index,
        task,
        max_files,
        entry_paths,
        "",
        coverage,
        cancel,
    )?;
    let mut used = 0;
    let mut text = String::new();
    let mut files = vec![];
    let mut truncated = vec![];
    let mut omitted = vec![];
    for path in array(&pack, "files").iter().filter_map(Value::as_str) {
        let api = index.file_api(path, 2000)?;
        let header = format!(
            "{path}{}\n",
            api["summary"]
                .as_str()
                .map(|s| format!("  # {}", s.split_whitespace().collect::<Vec<_>>().join(" ")))
                .unwrap_or_default()
        );
        if !truncated.is_empty() || !omitted.is_empty() || used + header.len() > max_bytes as usize
        {
            omitted.push(path.to_owned());
            continue;
        }
        let mut block = header;
        let mut shown = 0;
        for row in array(&api, "symbols") {
            let line = format!(
                "  {}  {}\n",
                n(row, "line"),
                s(row, "signature")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if used + block.len() + line.len() > max_bytes as usize {
                break;
            }
            block.push_str(&line);
            shown += 1;
        }
        if shown < n(&api, "total_symbols") {
            truncated.push(path.to_owned());
        }
        used += block.len();
        text.push_str(&block);
        let evidence = array(&pack, "evidence")
            .iter()
            .find(|r| s(r, "path") == path)
            .unwrap();
        files.push(json!({"path":path,"reason":evidence["reason"],"score":evidence["score"],"symbols":api["total_symbols"],"shown":shown}));
    }
    Ok(
        json!({"schema":1,"task":task,"project_graph_revision":pack["project_graph_revision"],"content_trust":"untrusted-source-data","note":"orientation: extracted declarations only, not evidence that code behaves as named","max_bytes":max_bytes,"bytes":used,"files":files,"truncated":truncated,"omitted":omitted,"retrieval":pack["retrieval"],"text":text}),
    )
}
pub fn text(pack: &Value) -> String {
    let mut out = format!(
        "task: {}\nproject_graph_revision: {}\n",
        s(pack, "task"),
        s(pack, "project_graph_revision")
    );
    for entry in array(pack, "entries") {
        out.push_str(&format!("entry: {}\n", entry.as_str().unwrap_or("")));
    }
    out.push_str("files:\n");
    for row in array(pack, "evidence") {
        out.push_str(&format!(
            "  {}  {}  {}\n",
            s(row, "path"),
            s(row, "reason"),
            n(row, "score")
        ));
    }
    for key in ["tests", "test_cases"] {
        out.push_str(&format!("{key}:\n"));
        for row in array(pack, key) {
            out.push_str(&format!(
                "  {}\n",
                row.as_str().unwrap_or_else(|| s(row, "id"))
            ));
        }
    }
    out.push_str(&format!(
        "pack_bytes: {}\npack_path: {}\n",
        n(pack, "pack_bytes"),
        s(pack, "pack_path")
    ));
    out
}
