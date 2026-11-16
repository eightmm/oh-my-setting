use super::{history, index::Graph};
use crate::{Error, Result, util::*};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn changed_paths(repo: &Path, base: &str, cancel: &AtomicBool) -> Result<Vec<String>> {
    let changes = workspace_changes(repo, base, cancel)?;
    Ok(array(&changes, "changed")
        .iter()
        .chain(array(&changes, "untracked"))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}
pub fn workspace_changes(repo: &Path, base: &str, cancel: &AtomicBool) -> Result<Value> {
    if base.starts_with('-') || base.contains('\0') {
        return Err(Error::new(format!("invalid base ref: {base}")));
    }
    let head = git(repo, &["rev-parse", "--verify", "HEAD"], cancel);
    let changed = if !base.is_empty() {
        let commit = git(
            repo,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
            cancel,
        );
        if commit.is_empty() || head.is_empty() {
            return Err(Error::new(format!("cannot resolve base and HEAD: {base}")));
        }
        let ancestor = git(repo, &["merge-base", &commit, &head], cancel);
        if ancestor.is_empty() {
            return Err(Error::new(format!(
                "base has no merge-base with HEAD: {base}"
            )));
        }
        git(repo, &["diff", "--name-only", &ancestor, "--"], cancel)
    } else if !head.is_empty() {
        git(repo, &["diff", "--name-only", "HEAD", "--"], cancel)
    } else {
        format!(
            "{}\n{}",
            git(repo, &["diff", "--name-only", "--cached", "--"], cancel),
            git(repo, &["diff", "--name-only", "--"], cancel)
        )
    };
    let paths: BTreeSet<String> = changed
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let untracked = git(
        repo,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancel,
    );
    let untracked: BTreeSet<_> = untracked.split('\0').filter(|s| !s.is_empty()).collect();
    Ok(json!({"changed":paths,"untracked":untracked}))
}
pub fn entries(repo: &Path, base: &str, head: &str, cancel: &AtomicBool) -> Result<Vec<Value>> {
    for (label, value) in [("base", base), ("head", head)] {
        if value.is_empty()
            || value.starts_with('-')
            || value.contains('\0')
            || git(
                repo,
                &["rev-parse", "--verify", &format!("{value}^{{tree}}")],
                cancel,
            )
            .is_empty()
        {
            return Err(Error::new(format!(
                "affected {label} is not a Git tree: {value}"
            )));
        }
    }
    let (status, raw, _) = command(
        repo,
        "git",
        &["diff", "--name-status", "-z", base, head, "--"],
        cancel,
    )?;
    if status != 0 {
        return Err(Error::new("affected Git diff is unavailable"));
    }
    let text = String::from_utf8_lossy(&raw);
    let fields: Vec<&str> = text.split('\0').collect();
    let mut pos = 0;
    let mut rows = vec![];
    while pos < fields.len() && !fields[pos].is_empty() {
        let status = fields[pos].chars().next().unwrap();
        pos += 1;
        if status == 'R' || status == 'C' {
            if pos + 1 >= fields.len() {
                return Err(Error::new(
                    "affected Git diff returned a truncated rename row",
                ));
            }
            rows.push(json!({"status":status.to_string(),"path":fields[pos+1].replace('\\',"/"),"old_path":fields[pos].replace('\\',"/")}));
            pos += 2;
        } else {
            if pos >= fields.len() {
                return Err(Error::new(
                    "affected Git diff returned a truncated path row",
                ));
            }
            rows.push(json!({"status":status.to_string(),"path":fields[pos].replace('\\',"/")}));
            pos += 1;
        }
    }
    rows.sort_by(|a, b| {
        (s(a, "path"), s(a, "status"), s(a, "old_path")).cmp(&(
            s(b, "path"),
            s(b, "status"),
            s(b, "old_path"),
        ))
    });
    Ok(rows)
}
fn boundary(path: &str, guards: &[String]) -> bool {
    [
        ".github/*",
        ".gitlab/*",
        "Jenkinsfile",
        "AGENTS.md",
        "**/AGENTS.md",
        "CLAUDE.md",
        "**/CLAUDE.md",
        "GEMINI.md",
        "**/GEMINI.md",
        "PROJECT.md",
        "**/PROJECT.md",
        "SECURITY.md",
        "**/SECURITY.md",
        "VERSION",
        "Makefile",
        "makefile",
        "GNUmakefile",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "tox.ini",
        "noxfile.py",
        "Cargo.toml",
        "go.mod",
        "go.sum",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "*.lock",
        "*.lock.json",
        "*-lock.json",
        "*lock.yaml",
        "*lock.yml",
        "install.sh",
        "**/install*.sh",
        "**/uninstall*.sh",
        "**/check.sh",
        "**/run-smoke-shard.sh",
    ]
    .iter()
    .any(|p| glob(p, path))
        || guards.iter().any(|p| glob(p, path))
}
fn runnable(path: &str, extra: &[String]) -> bool {
    extra.iter().any(|p| glob(p, path))
        || path.ends_with(".sh")
        || (path.rsplit('/').next().unwrap_or(path).starts_with("test_") && path.ends_with(".py"))
}
fn runner_edge(edge: &Value, graph: &Graph, extra: &[String]) -> bool {
    let (Some(source), Some(target)) = (
        graph.nodes.get(s(edge, "source")),
        graph.nodes.get(s(edge, "target")),
    ) else {
        return false;
    };
    ["calls", "tests"].contains(&s(edge, "relation"))
        && ["scripts/check.sh", "tests/run-smoke-shard.sh"].contains(&s(source, "path"))
        && s(target, "kind") == "test"
        && runnable(s(target, "path"), extra)
}
#[allow(clippy::too_many_arguments)]
pub fn report_with_helper(
    repo: &Path,
    graph: &Graph,
    base: &str,
    head: &str,
    depth: i64,
    guards: &[String],
    extra: &[String],
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<Value> {
    if depth < 0 {
        return Err(Error::new("affected depth must be zero or greater"));
    }
    let changes = entries(repo, base, head, cancel)?;
    let mut paths: Vec<String> = changes.iter().map(|r| s(r, "path").to_owned()).collect();
    let mut suites = BTreeMap::<String, Option<Vec<String>>>::new();
    let mut narrow = BTreeMap::<String, Vec<Value>>::new();
    let mut containment = BTreeSet::<String>::new();
    for change in &changes {
        if s(change, "status") != "M" {
            continue;
        }
        let path = s(change, "path");
        if !runnable(path, extra) {
            continue;
        }
        let hunks = super::cases::hunks(repo, base, head, path, cancel);
        let indexed = graph.by_path.contains_key(path);
        let before = super::cases::revision(repo, base, path, cancel);
        let after = super::cases::revision(repo, head, path, cancel);
        if !indexed {
            let names = if super::cases::SELECTABLE.contains(&path) {
                before
                    .as_deref()
                    .zip(after.as_deref())
                    .and_then(|(old, new)| super::cases::suite(old, new, &hunks))
            } else {
                None
            };
            suites.insert(path.to_owned(), names);
        } else if let Some((before, after)) = before.as_deref().zip(after.as_deref())
            && let Some(cases) =
                super::cases::changed(graph, path, &hunks, before, after, helper, cancel)
            && let Some(seeds) = super::cases::seeds(graph, path, &cases)
        {
            containment.extend(seeds);
            narrow.insert(path.to_owned(), cases);
        }
    }
    paths.retain(|path| !suites.contains_key(path));
    let documents: Vec<String> = paths
        .iter()
        .filter(|p| {
            [".md", ".mdx", ".rst", ".adoc"]
                .iter()
                .any(|e| p.to_lowercase().ends_with(e))
                && (p.starts_with("doc/")
                    || p.starts_with("docs/")
                    || ["README.", "CHANGELOG.", "CONTRIBUTING."]
                        .iter()
                        .any(|prefix| {
                            p.rsplit('/')
                                .next()
                                .unwrap_or(p)
                                .to_uppercase()
                                .starts_with(prefix)
                        }))
                && !boundary(p, guards)
        })
        .cloned()
        .collect();
    let seed_paths: Vec<String> = paths
        .iter()
        .filter(|path| !documents.contains(path) && !narrow.contains_key(*path))
        .cloned()
        .collect();
    let runner_paths: BTreeSet<_> = array(&graph.graph, "edges")
        .iter()
        .filter(|edge| runner_edge(edge, graph, extra))
        .filter_map(|edge| graph.nodes.get(s(edge, "target")))
        .map(|node| s(node, "path"))
        .collect();
    let code_paths: Vec<_> = paths
        .iter()
        .filter(|path| !documents.contains(path))
        .collect();
    let cut = !code_paths.is_empty()
        && code_paths
            .iter()
            .all(|path| runner_paths.contains(path.as_str()));
    let filtered;
    let index = if cut {
        let mut value = (*graph.graph).clone();
        value["edges"] = json!(
            array(&graph.graph, "edges")
                .iter()
                .filter(|edge| !runner_edge(edge, graph, extra))
                .collect::<Vec<_>>()
        );
        filtered = Graph::new(value)?;
        &filtered
    } else {
        graph
    };
    let relations = [
        "imports",
        "calls",
        "references",
        "tests",
        "uses",
        "depends_on",
        "reads_env",
    ];
    let mut seeds = containment.clone();
    for id in &containment {
        for edge in index.ins.get(id).into_iter().flatten() {
            if relations.contains(&s(edge, "relation"))
                && s(edge, "confidence") == "INFERRED"
                && graph
                    .nodes
                    .get(s(edge, "source"))
                    .is_some_and(|node| !narrow.contains_key(s(node, "path")))
            {
                seeds.insert(s(edge, "source").to_owned());
            }
        }
    }
    for (id, node) in &index.nodes {
        if s(node, "kind") == "module" && narrow.contains_key(s(node, "path")) {
            for edge in index.ins.get(id).into_iter().flatten() {
                if relations.contains(&s(edge, "relation"))
                    && ["EXTRACTED", "INFERRED"].contains(&s(edge, "confidence"))
                    && graph
                        .nodes
                        .get(s(edge, "source"))
                        .is_some_and(|node| !narrow.contains_key(s(node, "path")))
                {
                    seeds.insert(s(edge, "source").to_owned());
                }
            }
        }
    }
    let radius = index.blast(
        &seed_paths,
        if depth == 0 { None } else { Some(depth) },
        &["EXTRACTED"],
        &seeds.into_iter().collect::<Vec<_>>(),
    )?;
    let mut tests: Vec<String> = array(&radius, "tests")
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| runnable(p, extra))
        .map(str::to_owned)
        .collect();
    let mut cases: Vec<Value> = array(&radius, "test_cases")
        .iter()
        .filter(|row| tests.iter().any(|p| p == s(row, "path")))
        .cloned()
        .collect();
    let mut reasons = BTreeSet::new();
    let (diff, _, _) = command(repo, "git", &["diff", "--quiet", head, "--"], cancel)?;
    let (untracked, raw, _) = command(
        repo,
        "git",
        &["ls-files", "--others", "--exclude-standard"],
        cancel,
    )?;
    if diff != 0 || untracked != 0 || !raw.iter().all(u8::is_ascii_whitespace) {
        reasons.insert("workspace-differs-from-head".to_owned());
    }
    for path in array(&radius, "unmatched").iter().filter_map(Value::as_str) {
        if !documents.iter().any(|p| p == path) {
            reasons.insert(format!("unmatched:{path}"));
        }
    }
    for path in &paths {
        if boundary(path, guards) {
            reasons.insert(format!("boundary:{path}"));
        }
    }
    if radius["truncated"] == true {
        reasons.insert("depth-truncated".into());
    }
    for change in &changes {
        match s(change, "status") {
            "D" => {
                reasons.insert(format!("deleted:{}", s(change, "path")));
            }
            "R" | "C" => {
                reasons.insert(format!("renamed:{}", s(change, "path")));
            }
            _ => {}
        }
    }
    if !code_paths.is_empty() && tests.is_empty() {
        reasons.insert("no-tests".into());
    }
    let reached: BTreeSet<&str> = array(&radius, "seeds")
        .iter()
        .filter_map(Value::as_str)
        .chain(array(&radius, "dependents").iter().map(|row| s(row, "id")))
        .collect();
    if !tests.is_empty() && reasons.is_empty() {
        let mut covered: BTreeSet<&str> = reached
            .iter()
            .copied()
            .filter(|id| {
                tests
                    .iter()
                    .any(|path| path == s(&graph.nodes[*id], "path"))
            })
            .collect();
        let mut pending: Vec<_> = covered.iter().copied().collect();
        while let Some(id) = pending.pop() {
            for edge in graph.out.get(id).into_iter().flatten() {
                let target = s(edge, "target");
                if relations.contains(&s(edge, "relation"))
                    && s(edge, "confidence") == "EXTRACTED"
                    && reached.contains(target)
                    && covered.insert(target)
                {
                    pending.push(target);
                }
            }
        }
        let covered_paths: BTreeSet<_> = covered
            .iter()
            .map(|id| s(&graph.nodes[*id], "path"))
            .collect();
        for path in &code_paths {
            if !covered_paths.contains(path.as_str()) && !narrow.contains_key(path.as_str()) {
                reasons.insert(format!("no-tests:{path}"));
            }
        }
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for row in array(&radius, "dependents") {
        *counts.entry(s(row, "confidence").into()).or_default() += 1;
    }
    let mut ignored = BTreeMap::<String, usize>::new();
    let mut seen = BTreeSet::new();
    for id in &reached {
        for edge in graph.ins.get(id).into_iter().flatten() {
            let confidence = s(edge, "confidence");
            let key = (
                s(edge, "source"),
                s(edge, "target"),
                s(edge, "relation"),
                confidence,
            );
            if confidence != "EXTRACTED" && !reached.contains(s(edge, "source")) && seen.insert(key)
            {
                *ignored.entry(confidence.into()).or_default() += 1;
            }
        }
    }
    for (path, names) in &suites {
        if names.as_ref().is_none_or(|names| !names.is_empty()) {
            tests.push(path.clone());
        }
        cases.extend(super::cases::suite_rows(
            path,
            names.as_deref().unwrap_or(&[]),
        ));
    }
    tests.sort();
    tests.dedup();
    Ok(
        json!({"schema":1,"base":base,"head":head,"changes":changes,"mode":if reasons.is_empty(){"affected"}else{"full"},"depth":depth,"paths":paths,"documentation_paths":documents,"tests":tests,"test_cases":cases,"suite_paths":suites.keys().collect::<Vec<_>>(),"unsupported_test_count":array(&radius,"tests").len() as i64-tests.len() as i64,"confidence_counts":counts,"ignored_confidence_counts":ignored,"path_coverage":radius["path_coverage"],"unmatched":radius["unmatched"],"reasons":reasons}),
    )
}
pub fn limit_blast(value: &mut Value, limit: usize) {
    let mut omitted = json!({});
    for key in ["dependents", "files", "tests", "test_cases", "seeds"] {
        if let Some(rows) = value[key].as_array_mut()
            && rows.len() > limit
        {
            omitted[key] = json!(rows.len() - limit);
            rows.truncate(limit);
        }
    }
    value["limits"] = json!({"per_list":limit});
    value["truncated"] = json!(!omitted.as_object().unwrap().is_empty());
    value["omitted"] = omitted;
}
pub fn blast_text(value: &Value) -> String {
    let mut out = String::new();
    for path in array(value, "paths") {
        out.push_str(&format!("path: {}\n", path.as_str().unwrap_or("")));
    }
    out.push_str(&format!(
        "path_coverage: {}\ntruncated: {}\n",
        s(value, "path_coverage"),
        value["truncated"]
    ));
    for (key, label) in [("unmatched", "unmatched"), ("seeds", "seed")] {
        for row in array(value, key) {
            out.push_str(&format!("{label}: {}\n", row.as_str().unwrap_or("")));
        }
    }
    for row in array(value, "dependents") {
        out.push_str(&format!(
            "{}  {}  ({})\n",
            n(row, "distance"),
            s(row, "id"),
            s(row, "via")
        ));
    }
    for key in ["files", "tests", "test_cases"] {
        out.push_str(&format!("{key}:\n"));
        for row in array(value, key) {
            out.push_str(&format!(
                "  {}\n",
                row.as_str().unwrap_or_else(|| s(row, "id"))
            ));
        }
    }
    out
}
pub fn text(value: &Value) -> String {
    if s(value, "kind") == "impact-brief" {
        return impact_text(value);
    }
    let mut out = format!(
        "mode: {}\npath_coverage: {}\n",
        s(value, "mode"),
        s(value, "path_coverage")
    );
    if let Some(counts) = value["ignored_confidence_counts"].as_object() {
        let ordered: BTreeMap<_, _> = counts.iter().collect();
        for (confidence, count) in ordered {
            out.push_str(&format!("ignored_confidence: {confidence} {count}\n"));
        }
    }
    for (key, label) in [
        ("reasons", "reason"),
        ("documentation_paths", "documentation"),
        ("tests", "test"),
        ("test_cases", "test_case"),
    ] {
        for row in array(value, key) {
            out.push_str(&format!(
                "{label}: {}\n",
                row.as_str().unwrap_or_else(|| s(row, "id"))
            ));
        }
    }
    out
}
fn impact_text(value: &Value) -> String {
    let mut out = format!(
        "impact brief {}..{} ({})\n",
        s(value, "base"),
        s(value, "head"),
        s(value, "note")
    );
    for row in array(value, "changes") {
        out.push_str(&format!(
            "changed: {} {}\n",
            s(row, "status"),
            s(row, "path")
        ));
    }
    let affected = &value["affected"];
    out.push_str(&format!(
        "tests: mode={} path_coverage={}\n",
        s(affected, "mode"),
        s(affected, "path_coverage")
    ));
    for (key, label) in [
        ("reasons", "reason"),
        ("tests", "test"),
        ("test_cases", "test_case"),
        ("unmatched", "unmatched"),
    ] {
        for row in array(affected, key) {
            out.push_str(&format!("  {label}: {}\n", row.as_str().unwrap_or("")));
        }
    }
    for row in array(value, "documentation_paths") {
        out.push_str(&format!("documentation: {}\n", row.as_str().unwrap_or("")));
    }
    let dependents = &value["dependents"];
    out.push_str(&format!(
        "dependents ({}{}):\n",
        s(dependents, "basis"),
        if dependents["depth_truncated"] == true {
            ", deeper ones not followed"
        } else {
            ""
        }
    ));
    for row in array(dependents, "files") {
        out.push_str(&format!("  {}\n", row.as_str().unwrap_or("")));
    }
    let cochange = &value["co_change"];
    out.push_str(&format!(
        "usually changes with these but is not in this range ({}):\n",
        s(cochange, "basis")
    ));
    for row in array(cochange, "not_in_range") {
        out.push_str(&format!(
            "  {}  with {}  {:.1}%  shared {}  structural={}\n",
            s(row, "path"),
            s(row, "with"),
            row["degree"].as_f64().unwrap_or(0.0),
            n(row, "shared_revs"),
            if row["structural"] == true {
                "yes"
            } else {
                "no"
            }
        ));
    }
    if value["owners"].is_object() {
        out.push_str(&format!(
            "owners ({}, last matching rule):\n",
            s(&value["owners"], "file")
        ));
        for row in array(&value["owners"], "paths") {
            let owners = array(row, "owners")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!(
                "  {}  {}\n",
                s(row, "path"),
                if owners.is_empty() {
                    "(no owner)"
                } else {
                    &owners
                }
            ));
        }
    }
    if let Some(omitted) = value["omitted"].as_object() {
        let ordered: BTreeMap<_, _> = omitted.iter().collect();
        for (name, count) in ordered {
            out.push_str(&format!("omitted: {name} +{count} (raise --limit)\n"));
        }
    }
    out
}
#[allow(clippy::too_many_arguments)]
pub fn impact_with_helper(
    repo: &Path,
    graph: &Graph,
    base: &str,
    head: &str,
    depth: i64,
    commits: i64,
    limit: i64,
    cancel: &AtomicBool,
    helper: Option<&Path>,
) -> Result<Value> {
    if depth < 0 {
        return Err(Error::new("project impact --depth must be zero or greater"));
    }
    let changes = entries(repo, base, head, cancel)?;
    let paths: Vec<String> = changes.iter().map(|r| s(r, "path").to_owned()).collect();
    let mut changed: BTreeSet<String> = paths.iter().cloned().collect();
    changed.extend(
        changes
            .iter()
            .map(|r| s(r, "old_path"))
            .filter(|p| !p.is_empty())
            .map(str::to_owned),
    );
    let plan = report_with_helper(repo, graph, base, head, 0, &[], &[], cancel, helper)?;
    let radius = graph.blast(&paths, Some(depth), &["EXTRACTED"], &[])?;
    let dependents: Vec<Value> = array(&radius, "files")
        .iter()
        .filter(|p| {
            p.as_str().is_some_and(|p| {
                !changed.contains(p)
                    && !array(&radius, "tests")
                        .iter()
                        .any(|t| t.as_str() == Some(p))
            })
        })
        .cloned()
        .collect();
    let report = history::report(
        repo,
        graph,
        commits,
        50,
        5,
        30.0,
        &changed.iter().cloned().collect::<Vec<_>>(),
        0,
        cancel,
    )?;
    let mut partners = std::collections::BTreeMap::new();
    for row in array(&report, "pairs") {
        for (own, other) in [(s(row, "a"), s(row, "b")), (s(row, "b"), s(row, "a"))] {
            if changed.contains(own) && !changed.contains(other) {
                let candidate = json!({"path":other,"with":own,"degree":row["degree"],"shared_revs":row["shared_revs"],"structural":row["structural"]});
                if partners.get(other).is_none_or(|old: &Value| {
                    (row["degree"].as_f64().unwrap_or(0.0), n(row, "shared_revs"))
                        > (old["degree"].as_f64().unwrap_or(0.0), n(old, "shared_revs"))
                }) {
                    partners.insert(other.to_owned(), candidate);
                }
            }
        }
    }
    let mut partners: Vec<Value> = partners.into_values().collect();
    partners.sort_by(|a, b| {
        b["degree"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["degree"].as_f64().unwrap_or(0.0))
            .then_with(|| n(b, "shared_revs").cmp(&n(a, "shared_revs")))
            .then_with(|| s(a, "path").cmp(s(b, "path")))
    });
    let (owners_file, owners_rows) = codeowners(repo, &paths)?;
    let mut omitted = json!({});
    let mut cut = |name: &str, rows: &[Value]| {
        if rows.len() > limit as usize {
            omitted[name] = json!(rows.len() - limit as usize);
        }
        rows.iter()
            .take(limit as usize)
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut value = json!({"schema":1,"kind":"impact-brief","note":"orientation only: where to look, not evidence that the change is safe, tested or approved","base":base,"head":head,"project_graph_revision":graph.graph["revision"],"changes":cut("changes",&changes),"affected":{"mode":plan["mode"],"reasons":cut("affected.reasons",array(&plan,"reasons")),"tests":cut("affected.tests",array(&plan,"tests")),"test_cases":cut("affected.test_cases",&array(&plan,"test_cases").iter().map(|r|r["id"].clone()).collect::<Vec<_>>()),"path_coverage":plan["path_coverage"],"unmatched":cut("affected.unmatched",array(&plan,"unmatched"))},"documentation_paths":cut("documentation_paths",array(&plan,"documentation_paths")),"dependents":{"basis":format!("EXTRACTED reverse dependencies, depth {depth}"),"files":cut("dependents.files",&dependents),"depth_truncated":radius["truncated"]},"co_change":{"basis":format!("Git history, {} commit(s) read, {} bulk commit(s) skipped",n(&report,"commits"),n(&report,"skipped_bulk")),"not_in_range":cut("co_change.not_in_range",&partners)},"owners":if owners_file.is_empty(){Value::Null}else{json!({"file":owners_file,"paths":cut("owners.paths",&owners_rows)})},"limits":{"per_list":limit}});
    value["truncated"] = json!(!omitted.as_object().unwrap().is_empty());
    value["omitted"] = omitted;
    Ok(value)
}
fn owner_pattern(pattern: &str) -> Option<regex::Regex> {
    if pattern.is_empty() || pattern.starts_with('!') || pattern.contains(['[', '\\']) {
        return None;
    }
    let anchored = pattern.starts_with('/') || pattern.trim_end_matches('/').contains('/');
    let directory = pattern.ends_with('/');
    let body = pattern.trim_matches('/');
    if body.is_empty() {
        return regex::Regex::new(r"(?s).*\z").ok();
    }
    let mut regex = if anchored {
        String::new()
    } else {
        "(?:[^/]+/)*".into()
    };
    let mut rest = body;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix("**/") {
            regex.push_str("(?:[^/]+/)*");
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("**") {
            regex.push_str(".*");
            rest = tail;
        } else {
            let ch = rest.chars().next()?;
            match ch {
                '*' => regex.push_str("[^/]*"),
                '?' => regex.push_str("[^/]"),
                _ => regex.push_str(&regex::escape(&ch.to_string())),
            }
            rest = &rest[ch.len_utf8()..];
        }
    }
    if directory {
        regex.push_str("/.*");
    } else if !(body.rsplit('/').next().unwrap_or(body).contains('*') && !body.ends_with("**")) {
        regex.push_str("(?:/.*)?");
    }
    regex::Regex::new(&format!("(?s)^{regex}\\z")).ok()
}
fn codeowners(repo: &Path, paths: &[String]) -> Result<(String, Vec<Value>)> {
    let root = oms_safe_fs::Root::open(repo)
        .map_err(|e| Error::new(format!("owner source root: {e:?}")))?;
    for relative in [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"] {
        let Some(label) = oms_context_source::repo_label(repo, relative) else {
            continue;
        };
        let Ok(raw) = root.read(&label, 1024 * 1024, oms_safe_fs::Policy::source()) else {
            continue;
        };
        let text = String::from_utf8_lossy(&raw);
        let mut rules = vec![];
        for raw in text.lines() {
            let line = if raw.trim_start().starts_with('#') {
                ""
            } else {
                raw.split(" #").next().unwrap_or("").trim()
            };
            if line.is_empty() || line.starts_with(['[']) || line.starts_with("^[") {
                continue;
            }
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if let Some(pattern) = owner_pattern(fields[0]) {
                rules.push((fields[0], fields[1..].to_vec(), pattern));
            }
        }
        let rows = paths
            .iter()
            .map(|path| {
                let matching = rules
                    .iter()
                    .rev()
                    .find(|(_, _, pattern)| pattern.is_match(path));
                let (pattern, owners) = matching
                    .map(|(p, o, _)| (*p, o.clone()))
                    .unwrap_or(("", vec![]));
                json!({"path":path,"owners":owners,"pattern":pattern})
            })
            .collect();
        return Ok((relative.into(), rows));
    }
    Ok((String::new(), vec![]))
}

#[allow(clippy::too_many_arguments)]
pub fn report(
    repo: &Path,
    graph: &Graph,
    base: &str,
    head: &str,
    depth: i64,
    guards: &[String],
    extra: &[String],
    cancel: &AtomicBool,
) -> Result<Value> {
    report_with_helper(repo, graph, base, head, depth, guards, extra, cancel, None)
}
#[allow(clippy::too_many_arguments)]
pub fn impact(
    repo: &Path,
    graph: &Graph,
    base: &str,
    head: &str,
    depth: i64,
    commits: i64,
    limit: i64,
    cancel: &AtomicBool,
) -> Result<Value> {
    impact_with_helper(repo, graph, base, head, depth, commits, limit, cancel, None)
}
