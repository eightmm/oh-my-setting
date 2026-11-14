use super::index::Graph;
use crate::{Result, util::*};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn report(
    repo: &Path,
    graph: &Graph,
    commits: i64,
    max_changeset: i64,
    min_shared: i64,
    min_degree: f64,
    focus: &[String],
    limit: i64,
    cancel: &AtomicBool,
) -> Result<Value> {
    let present: BTreeSet<String> = git(repo, &["ls-files"], cancel)
        .lines()
        .map(str::to_owned)
        .collect();
    let window = commits.max(1).to_string();
    let raw = git(
        repo,
        &[
            "log",
            "-n",
            &window,
            "--no-merges",
            "--name-only",
            "--format=%x1e%H%x09%ct",
            "--",
            ".",
        ],
        cancel,
    );
    let mut sets = vec![];
    let mut bulk = 0;
    for chunk in raw.split('\u{1e}') {
        let mut lines = chunk.lines().map(str::trim).filter(|s| !s.is_empty());
        let Some(header) = lines.next() else {
            continue;
        };
        let (commit, stamp) = header.split_once('\t').unwrap_or((header, "0"));
        let files: BTreeSet<String> = lines
            .map(|s| s.replace('\\', "/"))
            .filter(|s| present.contains(s))
            .collect();
        if files.is_empty() {
            continue;
        }
        if files.len() as i128 > max_changeset as i128 {
            bulk += 1;
            continue;
        }
        sets.push(json!({"commit":commit,"time":stamp.parse::<i64>().unwrap_or(0),"files":files}));
    }
    let mut revisions = BTreeMap::<String, usize>::new();
    let mut shared = BTreeMap::<(String, String), usize>::new();
    for set in &sets {
        let files: Vec<String> = array(set, "files")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        for path in &files {
            *revisions.entry(path.clone()).or_default() += 1;
        }
        for i in 0..files.len() {
            for j in i + 1..files.len() {
                if !focus.is_empty() && !focus.contains(&files[i]) && !focus.contains(&files[j]) {
                    continue;
                }
                *shared
                    .entry((files[i].clone(), files[j].clone()))
                    .or_default() += 1;
            }
        }
    }
    let mut structural = BTreeSet::new();
    for edge in array(&graph.graph, "edges") {
        if s(edge, "relation") == "contains" {
            continue;
        }
        let a = graph
            .nodes
            .get(s(edge, "source"))
            .map(|n| s(n, "path"))
            .unwrap_or("");
        let b = graph
            .nodes
            .get(s(edge, "target"))
            .map(|n| s(n, "path"))
            .unwrap_or("");
        if !a.is_empty() && !b.is_empty() && a != b {
            structural.insert((a.min(b).to_owned(), a.max(b).to_owned()));
        }
    }
    let mut rows = vec![];
    for ((a, b), count) in shared {
        if (count as i128) < min_shared as i128 {
            continue;
        }
        let degree = (2000.0 * count as f64 / (revisions[&a] + revisions[&b]) as f64)
            .round_ties_even()
            / 10.0;
        if degree < min_degree {
            continue;
        }
        let known = structural.contains(&(a.clone(), b.clone()));
        rows.push(json!({"a":a,"b":b,"shared_revs":count,"revs":[revisions[&a],revisions[&b]],"degree":degree,"structural":known}));
    }
    rows.sort_by(|a, b| {
        b["degree"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["degree"].as_f64().unwrap_or(0.0))
            .then_with(|| n(b, "shared_revs").cmp(&n(a, "shared_revs")))
            .then_with(|| (s(a, "a"), s(a, "b")).cmp(&(s(b, "a"), s(b, "b"))))
    });
    let total = rows.len();
    let hidden = rows.iter().filter(|r| r["structural"] != true).count();
    if limit != 0 {
        rows.truncate(limit.max(1) as usize);
    }
    let mut focus = focus.to_vec();
    focus.sort();
    focus.dedup();
    Ok(
        json!({"schema":1,"commits":sets.len(),"skipped_bulk":bulk,"params":{"commits":commits,"max_changeset":max_changeset,"min_shared":min_shared,"min_degree":min_degree,"focus":focus},"graph_revision":graph.graph["revision"],"pairs":rows,"hidden":hidden,"truncated":rows.len()<total,"omitted":total-rows.len()}),
    )
}
pub fn text(report: &Value) -> String {
    let mut out = format!(
        "coupling: {} commit(s), {} bulk skipped\n",
        n(report, "commits"),
        n(report, "skipped_bulk")
    );
    for row in array(report, "pairs") {
        out.push_str(&format!(
            "{:.1}%  shared {}  {}  {}  structural={}\n",
            row["degree"].as_f64().unwrap_or(0.0),
            n(row, "shared_revs"),
            s(row, "a"),
            s(row, "b"),
            if row["structural"] == true {
                "yes"
            } else {
                "no"
            }
        ));
    }
    out
}
