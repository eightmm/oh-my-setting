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
        if !path.to_lowercase().ends_with(".ipynb")
            && let Ok(data) = root.read(path, 2 * 1024 * 1024, oms_safe_fs::Policy::source())
            && sha(&data) == s(node, "source_digest")
        {
            lines = String::from_utf8_lossy(&data)
                .lines()
                .map(str::to_owned)
                .collect();
        }
        sources.insert(path.into(), lines);
    }
    let quoted =
        regex::Regex::new(r#"'([^'\\]*(?:\\.[^'\\]*)*)'|"([^"\\]*(?:\\.[^"\\]*)*)""#).unwrap();
    let mut docs = vec![];
    let mut inverted: BTreeMap<String, Vec<(usize, [usize; 4])>> = BTreeMap::new();
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
        let vocabulary: BTreeSet<&str> = counters
            .iter()
            .flat_map(|c| c.keys().map(String::as_str))
            .collect();
        for term in vocabulary {
            inverted.entry(term.to_owned()).or_default().push((
                docs.len(),
                std::array::from_fn(|f| *counters[f].get(term).unwrap_or(&0)),
            ));
        }
        docs.push((node, lengths));
    }
    let count = docs.len();
    let averages: Vec<f64> = (0..4)
        .map(|f| docs.iter().map(|d| d.1[f]).sum::<usize>() as f64 / count.max(1) as f64)
        .collect();
    let mut scores = vec![0.0; count];
    for term in &unique {
        let Some(postings) = inverted.get(term.as_str()) else {
            continue;
        };
        let frequency = postings.len() as f64;
        let weight = (1.0 + (count as f64 - frequency + 0.5) / (frequency + 0.5)).ln();
        for (i, frequencies) in postings {
            for field in 0..4 {
                let tf = frequencies[field] as f64;
                if tf > 0.0 {
                    let norm =
                        1.2 * (0.25 + 0.75 * docs[*i].1[field] as f64 / averages[field].max(1.0));
                    scores[*i] += [3.0, 1.0, 1.5, 0.6][field] * weight * tf * 2.2 / (tf + norm);
                }
            }
        }
    }
    let mut rows = vec![];
    for ((node, _), score) in docs.iter().zip(scores) {
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
#[allow(clippy::too_many_arguments)]
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
        .filter(|w| !stop.contains(&super::unicode::casefold(w).as_str()))
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
    let change = if base.is_empty() {
        json!({"changed":[],"untracked":[]})
    } else {
        super::affected::workspace_changes(repo, base, cancel)?
    };
    let change_paths: Vec<String> = array(&change, "changed")
        .iter()
        .chain(array(&change, "untracked"))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for path in &change_paths {
        if index.by_path.contains_key(path) {
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
    cases.truncate(max_files.max(0).saturating_mul(4).min(40) as usize);
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
    let mut pack = json!({"task":task,"project_graph_revision":index.graph["revision"],"entries":entries.iter().map(|r|s(r,"id")).collect::<Vec<_>>(),"files":files,"tests":tests,"test_cases":cases,"blast":{"seeds":array(&blast,"seeds").iter().take(40).collect::<Vec<_>>(),"dependents":array(&blast,"dependents").iter().take(40).collect::<Vec<_>>(),"files":array(&blast,"files").iter().take(max_files.max(0)as usize).collect::<Vec<_>>(),"tests":tests,"test_cases":cases,"unmatched":blast["unmatched"],"path_coverage":blast["path_coverage"],"truncated":blast["truncated"],"counts":{"seeds":array(&blast,"seeds").len(),"dependents":array(&blast,"dependents").len(),"files":array(&blast,"files").len(),"tests":array(&blast,"tests").len(),"test_cases":array(&blast,"test_cases").len()}},"hubs":hubs,"evidence":evidence,"assurance":{"schema":1,"basis":all["basis"],"note":all["note"],"counts":assurance_counts,"nodes":assurance_rows},"change":{"schema":1,"enabled":!base.is_empty(),"base":base,"impact_basis":"EXTRACTED reverse dependencies","changed":change["changed"],"untracked":change["untracked"],"impacted":impacted,"unmatched":change_blast["unmatched"],"truncated":change_blast["truncated"]},"coverage":coverage,"pack_bytes":0,"retrieval":{"method":if entry_paths.is_empty(){"lexical"}else{"entry-path"},"status":status,"unmatched_entry_paths":unmatched,"hint":if status=="matched"{""}else{"No complete match. Search source identifiers/paths directly, then use --entry-path. Lexical search does not translate the task; missing matches do not prove missing code."}}});
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
#[allow(clippy::too_many_arguments)]
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
    let mut budget_exhausted = false;
    for path in array(&pack, "files").iter().filter_map(Value::as_str) {
        if budget_exhausted {
            omitted.push(path.to_owned());
            continue;
        }
        let api = index.file_api(path, 2000)?;
        let header = format!(
            "{path}{}\n",
            api["summary"]
                .as_str()
                .map(|s| format!("  # {}", s.split_whitespace().collect::<Vec<_>>().join(" ")))
                .unwrap_or_default()
        );
        if used + header.len() > max_bytes as usize {
            budget_exhausted = true;
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
                budget_exhausted = true;
                break;
            }
            block.push_str(&line);
            shown += 1;
        }
        let clipped = array(&api, "symbols")
            .iter()
            .any(|r| r["signature_truncated"] == true || r["scope_truncated"] == true);
        if shown < n(&api, "total_symbols") || clipped {
            truncated.push(path.to_owned());
        }
        used += block.len();
        text.push_str(&block);
        let evidence = array(&pack, "evidence")
            .iter()
            .find(|r| s(r, "path") == path)
            .unwrap();
        let incomplete_spans = array(&api, "symbols")
            .iter()
            .filter(|r| r["span_complete"] == false)
            .count();
        files.push(json!({"path":path,"reason":evidence["reason"],"score":evidence["score"],"symbols":api["total_symbols"],"shown":shown,"incomplete_spans":incomplete_spans}));
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

/// The canonical runtime compiler owns phase selection, source slicing and the
/// evidence-envelope digest. Its producer services remain supplied by the host.
pub fn compile_bundle_with_services(
    repo: &Path,
    state_repo: &Path,
    pack: &Value,
    max_bytes: usize,
    services: &dyn oms_runtime::Services,
    cancel: &AtomicBool,
) -> Result<Value> {
    compile_bundle_with_host(repo, state_repo, pack, max_bytes, None, services, cancel)
}

/// Compile a runtime context bundle using an optional host-supplied install root.
pub fn compile_bundle_with_host(
    repo: &Path,
    state_repo: &Path,
    pack: &Value,
    max_bytes: usize,
    install_root: Option<&Path>,
    services: &dyn oms_runtime::Services,
    cancel: &AtomicBool,
) -> Result<Value> {
    let install = install_root
        .map(Path::to_path_buf)
        .or_else(crate::execution::install_root)
        .ok_or_else(|| Error::new("packaged OMS installation root was not supplied"))?;
    let runtime =
        oms_runtime::storage::Runtime::open(repo, state_repo, &install, services, cancel)?;
    let request = oms_runtime::context::ContextRequest {
        targets: vec![],
        explicit: array(pack, "evidence")
            .iter()
            .filter_map(|item| {
                let path = s(item, "path");
                (!path.is_empty()).then(|| {
                    (
                        path.into(),
                        format!(
                            "project-graph: {}",
                            item.get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("selected")
                        ),
                    )
                })
            })
            .collect(),
        required: vec![],
        max_bytes,
        bundle: None,
        manifest: None,
        allow_external: false,
        phase: "implementation".into(),
    };
    let manifest = oms_runtime::context::plan(&runtime, &request)?;
    Ok(serde_json::from_slice(&oms_runtime::value::canonical(
        &manifest,
    )?)?)
}
pub(crate) struct LocalRuntime<'a>(pub &'a AtomicBool);
impl oms_runtime::Services for LocalRuntime<'_> {
    fn repository_view(
        &self,
        repo: &Path,
    ) -> oms_runtime::Result<oms_runtime::services::RepositoryView> {
        Ok(oms_runtime::services::RepositoryView {
            head: git(repo, &["rev-parse", "HEAD"], self.0),
            branch: git(repo, &["symbolic-ref", "--short", "HEAD"], self.0),
            worktree_state: oms_command_task::state_fingerprint(repo, self.0).map_err(|e| {
                oms_command_plan::Error {
                    code: e.code,
                    message: String::from_utf8_lossy(&e.message).into_owned(),
                }
            })?,
        })
    }
}
pub fn compile_bundle(
    repo: &Path,
    pack: &Value,
    max_bytes: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    compile_bundle_with_root(repo, pack, max_bytes, None, cancel)
}

pub fn compile_bundle_with_root(
    repo: &Path,
    pack: &Value,
    max_bytes: usize,
    install_root: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut request = oms_command_plan::Request::new(oms_command_plan::Action::Status);
    request.repo = Some(repo.into());
    let context = oms_command_plan::Context::from_environment(&request, repo)?;
    compile_bundle_with_host(
        repo,
        &context.state_repo,
        pack,
        max_bytes,
        install_root,
        &LocalRuntime(cancel),
        cancel,
    )
}

/// Validate a context pack before passing its contents to a provider.
pub fn validate_context_pack(path: &Path, repo: &Path) -> Result<Value> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| Error::new("invalid pack path"))?;
    let root = oms_safe_fs::Root::open(parent)
        .map_err(|e| Error::new(format!("cannot open pack directory: {e:?}")))?;
    let raw = root
        .read_os(name, 262144, oms_safe_fs::Policy::source())
        .map_err(|e| Error::new(format!("cannot read pack: {e:?}")))?;
    let reviewed =
        oms_command_plan::reviewed_context::from_pack(&raw).map_err(|e| Error::new(e.message))?;
    if let Some(excerpt) = &reviewed {
        excerpt
            .check_project(repo)
            .map_err(|e| Error::new(e.message))?;
    }
    std::str::from_utf8(&raw).map_err(|_| Error::new("pack is not UTF-8"))?;
    use std::os::unix::ffi::OsStrExt;
    let mut env = Vec::new();
    let mut total = 0usize;
    for (key, value) in std::env::vars_os() {
        total = total
            .saturating_add(key.as_bytes().len())
            .saturating_add(value.as_bytes().len());
        if env.len() >= oms_scrub::MAX_ENV_ENTRIES || total > oms_scrub::MAX_ENV_BYTES {
            return Err(Error::new("outbound environment exceeds scan limits"));
        }
        env.push((key.as_bytes().to_vec(), value.as_bytes().to_vec()));
    }
    let refs: Vec<_> = env
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    fn scan(raw: &[u8], env: &[(&[u8], &[u8])]) -> Result<()> {
        if oms_scrub::scan(raw, env).map_or(true, |hits| !hits.is_empty()) {
            return Err(Error::new("pack contains sensitive content"));
        }
        Ok(())
    }
    scan(&raw, &refs)?;
    let pack: Value =
        serde_json::from_slice(&raw).map_err(|_| Error::new("pack is not valid JSON"))?;
    let absolute =
        regex::Regex::new(r"(?:^|[^A-Za-z0-9_.:/-])(?:/[^/\s]+/[^\s]+|[A-Za-z]:[\\/][^\s]+)")
            .unwrap();
    fn inspect(value: &Value, env: &[(&[u8], &[u8])], absolute: &regex::Regex) -> Result<()> {
        fn text(value: &str, env: &[(&[u8], &[u8])], absolute: &regex::Regex) -> Result<()> {
            scan(value.as_bytes(), env)?;
            if absolute.is_match(value) {
                return Err(Error::new("pack contains absolute path content"));
            }
            Ok(())
        }
        match value {
            Value::String(value) => text(value, env, absolute)?,
            Value::Array(rows) => {
                for row in rows {
                    inspect(row, env, absolute)?;
                }
            }
            Value::Object(rows) => {
                for (key, value) in rows {
                    text(key, env, absolute)?;
                    inspect(value, env, absolute)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    inspect(&pack, &refs, &absolute)?;
    if !pack.is_object() {
        return Err(Error::new("pack must be a JSON object"));
    }
    let digest = pack["pack_digest"].as_str().unwrap_or("");
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::new(
            "pack_digest must be 64 lowercase hex characters",
        ));
    }
    fn path_value(v: &Value, label: &str) -> Result<String> {
        let p = v
            .as_str()
            .ok_or_else(|| Error::new(format!("{label} path must be a string")))?;
        if p.is_empty()
            || p.chars().count() > 400
            || p.starts_with('/')
            || p.as_bytes().get(1) == Some(&b':')
            || p.contains(['\\', '\0', '\n', '\r'])
            || p.starts_with("./")
            || p.split('/').any(|s| s == "..")
            || matches!(p.split('/').next(), Some(".git" | ".oms"))
        {
            return Err(Error::new(format!("{label} contains an invalid path")));
        }
        Ok(p.into())
    }
    fn rows<'a>(pack: &'a Value, key: &str) -> Result<&'a [Value]> {
        match pack.get(key) {
            None => Ok(&[]),
            Some(v) => {
                let a = v
                    .as_array()
                    .ok_or_else(|| Error::new(format!("{key} must be a list")))?;
                if a.len() > 200 {
                    return Err(Error::new(format!("{key} exceeds the entry cap")));
                }
                Ok(a)
            }
        }
    }
    let files = rows(&pack, "files")?
        .iter()
        .map(|v| path_value(v, "files"))
        .collect::<Result<Vec<_>>>()?;
    let tests = rows(&pack, "tests")?
        .iter()
        .map(|v| path_value(v, "tests"))
        .collect::<Result<Vec<_>>>()?;
    let mut cases = Vec::new();
    for v in rows(&pack, "test_cases")? {
        if !v.is_object() {
            return Err(Error::new("test_cases entries must be objects"));
        }
        let mut row = json!({"path": path_value(&v["path"], "test_cases")?});
        for key in ["id", "language", "name"] {
            let value = v[key]
                .as_str()
                .ok_or_else(|| Error::new(format!("test_cases {key} must be a string")))?;
            if value.chars().count() > 400 || value.contains(['\0', '\r', '\n']) {
                return Err(Error::new(format!(
                    "test_cases {key} is not a bounded line"
                )));
            }
            row[key] = json!(value);
        }
        cases.push(row);
    }
    let mut evidence = Vec::new();
    for v in rows(&pack, "evidence")? {
        if !v.is_object() {
            return Err(Error::new("evidence entries must be objects"));
        }
        let reason = v
            .get("reason")
            .map_or(Some(""), Value::as_str)
            .ok_or_else(|| Error::new("evidence reason must be a string"))?;
        if reason.contains(['\0', '\r', '\n']) {
            return Err(Error::new("evidence reason contains a control character"));
        }
        evidence.push(json!({"path": path_value(&v["path"], "evidence")?, "reason": reason.chars().take(200).collect::<String>()}));
    }
    if pack.get("hubs").is_some_and(|v| !v.is_array()) {
        return Err(Error::new("hubs must be a list"));
    }
    let revision = pack
        .get("project_graph_revision")
        .map_or(Some(""), Value::as_str)
        .ok_or_else(|| Error::new("project_graph_revision must be a string"))?;
    if revision.chars().count() > 400 || revision.contains(['\0', '\r', '\n']) {
        return Err(Error::new(
            "project_graph_revision is not a single bounded line",
        ));
    }
    let mut view = json!({"schema":1,"path":path.to_string_lossy(),"pack_digest":digest,"project_graph_revision":revision,"file_count":files.len(),"files":files,"tests":tests,"test_cases":cases,"evidence":evidence,"sha256":sha(&raw)});
    if reviewed.is_some() {
        view["lead_findings"] = pack["lead_findings"].clone();
    }
    Ok(view)
}

#[cfg(test)]
mod bridge_pack_tests {
    use super::*;
    #[test]
    fn environment_value_is_refused() {
        let value = "uniquevaluefortheoutboundscan987654321";
        if std::env::var_os("OMS_BRIDGE_TEST_CHILD").is_some() {
            let dir = std::env::temp_dir().join(format!("oms-bridge-env-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("pack.json");
            std::fs::write(
                &path,
                json!({"pack_digest":"a".repeat(64), "task":value}).to_string(),
            )
            .unwrap();
            assert!(validate_context_pack(&path, &dir).is_err());
            std::fs::remove_dir_all(dir).unwrap();
        } else {
            let key = ["OMS_BRIDGE_", "TO", "KEN"].concat();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "project::context::bridge_pack_tests::environment_value_is_refused",
                ])
                .env("OMS_BRIDGE_TEST_CHILD", "1")
                .env(key, value)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }
    #[test]
    fn pack_accepts_and_refuses() {
        let dir = std::env::temp_dir().join(format!("oms-bridge-pack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pack.json");
        let pack = json!({"pack_digest":"a".repeat(64), "files":["src/a.py"], "test_cases":[], "evidence":[]});
        std::fs::write(&path, pack.to_string()).unwrap();
        assert_eq!(validate_context_pack(&path, &dir).unwrap()["file_count"], 1);
        std::fs::write(&path, "{").unwrap();
        assert!(validate_context_pack(&path, &dir).is_err());
        let unsafe_text = ["pass", "word", ": bad"].concat();
        std::fs::write(&path, unsafe_text).unwrap();
        assert!(validate_context_pack(&path, &dir).is_err());
        std::fs::write(&path, json!({"pack_digest":"x".repeat(64)}).to_string()).unwrap();
        assert!(validate_context_pack(&path, &dir).is_err());
        let base = json!({"pack_digest":"a".repeat(64), "task":"inspect parser references", "hubs":[], "coverage":{"parsed":5}, "files":[], "tests":[]});
        std::fs::write(&path, base.to_string()).unwrap();
        assert!(validate_context_pack(&path, &dir).is_ok());
        for bad in [
            json!({"project_graph_revision":"bad\0line"}),
            json!({"hubs":{}}),
            json!({"files":["../escape"]}),
            json!({"files":vec!["x";201]}),
        ] {
            let mut pack = base.clone();
            for (key, value) in bad.as_object().unwrap() {
                pack[key] = value.clone();
            }
            std::fs::write(&path, pack.to_string()).unwrap();
            assert!(validate_context_pack(&path, &dir).is_err());
        }
        let mut at_cap = base.clone();
        at_cap["files"] = json!(vec!["x"; 200]);
        std::fs::write(&path, at_cap.to_string()).unwrap();
        assert_eq!(
            validate_context_pack(&path, &dir).unwrap()["file_count"],
            200
        );
        let text = "Reviewed finding in src/a.py.";
        let findings = json!({"schema":1,"kind":"reviewed-lead-answer","authority":"context-only",
            "source":{"project_key":oms_command_plan::reviewed_context::project_key(&dir).unwrap(),"room":"review-room","participant":"ask-fixture","provider":"claude","lead_request_id":"lead-fixture","turn_id":"turn-fixture","operation":"ask-fixture","answer_sha256":"a".repeat(64),"page_offset":0,"page_bytes":text.len(),"page_cursor_sha256":sha(""),"answer_bytes":text.len()},
            "selection":{"text":text,"offset":0,"bytes":text.len(),"sha256":sha(text)},
            "target":{"task_id":"reviewed-task","task_sha256":"b".repeat(64),"contract_sha256":"c".repeat(64),"provider":"claude"}});
        let pack = |finding: &Value| {
            let digest = oms_command_plan::json::hash(
                &oms_command_plan::json::parse(finding.to_string().as_bytes()).unwrap(),
            )
            .unwrap();
            json!({"pack_digest":digest,"files":[],"lead_findings":finding})
        };
        std::fs::write(&path, pack(&findings).to_string()).unwrap();
        assert_eq!(
            validate_context_pack(&path, &dir).unwrap()["lead_findings"],
            findings
        );
        for change in [
            "version",
            "oversized",
            "range",
            "digest",
            "project",
            "extra",
        ] {
            let mut finding = findings.clone();
            match change {
                "version" => finding["schema"] = json!(2),
                "oversized" => {
                    let text = "a".repeat(4097);
                    finding["selection"] =
                        json!({"text":text,"offset":0,"bytes":4097,"sha256":sha(&text)});
                }
                "range" => finding["selection"]["offset"] = json!(1),
                "digest" => finding["selection"]["sha256"] = json!("f".repeat(64)),
                "project" => finding["source"]["project_key"] = json!("f".repeat(64)),
                _ => finding["authorityOverride"] = json!(true),
            }
            std::fs::write(&path, pack(&finding).to_string()).unwrap();
            assert!(validate_context_pack(&path, &dir).is_err(), "{change}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
