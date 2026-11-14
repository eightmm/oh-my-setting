use super::{history, index::Graph};
use crate::{Error, Result, util::*};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn changed_paths(repo: &Path, base: &str, cancel: &AtomicBool) -> Result<Vec<String>> {
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
    let mut paths: BTreeSet<String> = changed
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let untracked = git(
        repo,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        cancel,
    );
    paths.extend(
        untracked
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    );
    Ok(paths.into_iter().collect())
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
    let name = path.rsplit('/').next().unwrap_or(path);
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
    .any(|p| glob(p, path) || glob(p, name))
        || guards.iter().any(|p| glob(p, path))
}
fn runnable(path: &str, extra: &[String]) -> bool {
    extra.iter().any(|p| glob(p, path))
        || path.ends_with(".sh")
        || (path.rsplit('/').next().unwrap_or(path).starts_with("test_") && path.ends_with(".py"))
}
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
    if depth < 0 {
        return Err(Error::new("affected depth must be zero or greater"));
    }
    let changes = entries(repo, base, head, cancel)?;
    let paths: Vec<String> = changes.iter().map(|r| s(r, "path").to_owned()).collect();
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
        .filter(|p| !documents.contains(*p))
        .cloned()
        .collect();
    let radius = graph.blast(
        &seed_paths,
        if depth == 0 { None } else { Some(depth) },
        &["EXTRACTED"],
        &[],
    )?;
    let tests: Vec<&str> = array(&radius, "tests")
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| runnable(p, extra))
        .collect();
    let cases: Vec<Value> = array(&radius, "test_cases")
        .iter()
        .filter(|r| tests.contains(&s(r, "path")))
        .cloned()
        .collect();
    let mut reasons = BTreeSet::new();
    let exact_head = git(repo, &["rev-parse", "--verify", "HEAD^{tree}"], cancel);
    let requested = git(
        repo,
        &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
        cancel,
    );
    let dirty = git(
        repo,
        &["status", "--porcelain", "--untracked-files=all"],
        cancel,
    );
    if exact_head != requested || !dirty.is_empty() {
        reasons.insert("workspace-differs-from-head".to_owned());
    }
    for path in array(&radius, "unmatched").iter().filter_map(Value::as_str) {
        reasons.insert(format!("unmatched:{path}"));
    }
    for path in &paths {
        if boundary(path, guards) {
            reasons.insert(format!("boundary:{path}"));
        }
    }
    if radius["truncated"] == true {
        reasons.insert("depth-truncated".into());
    }
    for row in &changes {
        match s(row, "status") {
            "D" => {
                reasons.insert(format!("deleted:{}", s(row, "path")));
            }
            "R" | "C" => {
                reasons.insert(format!("renamed:{}", s(row, "path")));
            }
            _ => {}
        }
    }
    if !seed_paths.is_empty() && tests.is_empty() {
        reasons.insert("no-tests".into());
    }
    // A successful test link for one changed file cannot certify another.
    for path in &seed_paths {
        let individual = graph.blast(std::slice::from_ref(path), None, &["EXTRACTED"], &[])?;
        if !array(&individual, "tests")
            .iter()
            .filter_map(Value::as_str)
            .any(|p| runnable(p, extra))
            && !tests.is_empty()
        {
            reasons.insert(format!("no-tests:{path}"));
        }
    }
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for row in array(&radius, "dependents") {
        *counts.entry(s(row, "confidence").into()).or_default() += 1;
    }
    let reached: BTreeSet<&str> = array(&radius, "seeds")
        .iter()
        .filter_map(Value::as_str)
        .chain(array(&radius, "dependents").iter().map(|r| s(r, "id")))
        .collect();
    let mut ignored = std::collections::BTreeMap::<String, usize>::new();
    let mut seen = BTreeSet::new();
    for id in &reached {
        for edge in graph.ins.get(*id).into_iter().flatten() {
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
    Ok(
        json!({"schema":1,"mode":if reasons.is_empty(){"affected"}else{"full"},"depth":depth,"paths":paths,"documentation_paths":documents,"tests":tests,"test_cases":cases,"suite_paths":[],"unsupported_test_count":array(&radius,"tests").len()-tests.len(),"confidence_counts":counts,"ignored_confidence_counts":ignored,"path_coverage":radius["path_coverage"],"unmatched":radius["unmatched"],"reasons":reasons}),
    )
}
pub fn limit_blast(value: &mut Value, limit: usize) {
    let mut counts = json!({});
    let mut omitted = json!({});
    for key in ["dependents", "files", "tests", "test_cases"] {
        let rows = value[key].as_array_mut().unwrap();
        counts[key] = json!(rows.len());
        if rows.len() > limit {
            omitted[key] = json!(rows.len() - limit);
            rows.truncate(limit);
        }
    }
    value["counts"] = counts;
    value["omitted"] = omitted;
}
pub fn blast_text(value: &Value) -> String {
    let mut out = String::new();
    for key in ["seeds", "dependents", "files", "tests", "unmatched"] {
        for row in array(value, key) {
            out.push_str(&format!(
                "{}: {}\n",
                key,
                row.as_str().unwrap_or_else(|| s(row, "id"))
            ));
        }
    }
    out
}
pub fn text(value: &Value) -> String {
    if s(value, "kind") == "impact-brief" {
        return format!(
            "impact brief {}..{} ({})\n{}",
            s(value, "base"),
            s(value, "head"),
            s(value, "note"),
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    }
    let mut out = format!(
        "mode: {}\npath_coverage: {}\n",
        s(value, "mode"),
        s(value, "path_coverage")
    );
    for key in [
        "reasons",
        "documentation_paths",
        "tests",
        "test_cases",
        "unmatched",
    ] {
        for row in array(value, key) {
            out.push_str(&format!(
                "{key}: {}\n",
                row.as_str().unwrap_or_else(|| s(row, "id"))
            ));
        }
    }
    out
}
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
    let plan = report(repo, graph, base, head, 0, &[], &[], cancel)?;
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
