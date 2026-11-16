//! Fail-open changed-case selection. An uncertain range selects the whole file.
use super::{build, index::Graph};
use crate::util::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const SELECTABLE: &[&str] = &["tests/scripts-smoke.sh", "tests/autonomy-plan-run-smoke.sh"];
pub fn hunks(repo: &Path, base: &str, head: &str, path: &str, cancel: &AtomicBool) -> Vec<Value> {
    let Ok((0, raw, _)) = command(
        repo,
        "git",
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--unified=0",
            base,
            head,
            "--",
            &format!(":(literal){path}"),
        ],
        cancel,
    ) else {
        return vec![];
    };
    let Ok(text) = std::str::from_utf8(&raw) else {
        return vec![];
    };
    let pattern = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap();
    let mut result: Vec<Value> = vec![];
    let mut active = false;
    let (mut old, mut new) = (0_u64, 0_u64);
    for line in text.split('\n') {
        if line.starts_with("diff --git ") {
            active = false;
        }
        if let Some(c) = pattern.captures(line) {
            let number = |i| {
                c.get(i)
                    .map(|m| m.as_str().parse::<u64>().unwrap_or(0))
                    .unwrap_or(1)
            };
            old = number(1);
            new = number(3);
            result.push(json!({"old_start":old,"old_count":number(2),"new_start":new,"new_count":number(4),"old_lines":[],"new_lines":[]}));
            active = true;
        } else if active {
            let row = result.last_mut().unwrap();
            if let Some(body) = line.strip_prefix('-').filter(|_| !line.starts_with("---")) {
                row["old_lines"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!([old, body]));
                old += 1;
            } else if let Some(body) = line.strip_prefix('+').filter(|_| !line.starts_with("+++")) {
                row["new_lines"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!([new, body]));
                new += 1;
            } else if line.starts_with(' ') {
                old += 1;
                new += 1;
            }
        }
    }
    result
}
pub fn revision(repo: &Path, revision: &str, path: &str, cancel: &AtomicBool) -> Option<String> {
    let (exit, raw, _) = command(
        repo,
        "git",
        &["show", &format!("{revision}:{path}")],
        cancel,
    )
    .ok()?;
    (exit == 0).then(|| String::from_utf8(raw).ok()).flatten()
}
fn ranges(nodes: &[Value], path: &str, text: &str) -> Option<BTreeMap<(String, String), Value>> {
    let lines: Vec<_> = text.split('\n').collect();
    let mut ranges = BTreeMap::new();
    for node in nodes {
        if s(node, "path") != path
            || !["function", "method"].contains(&s(node, "kind"))
            || !s(node, "name").starts_with("test_")
            || !["python", "shell"].contains(&s(node, "language"))
        {
            continue;
        }
        let definition = node["metadata"]["line"].as_u64()? as usize;
        let end = node["metadata"]["end_line"].as_u64()? as usize;
        if definition < 1 || end < definition || end > lines.len() {
            return None;
        }
        let mut start = definition;
        while start > 1
            && lines[start - 2].trim_start().starts_with('#')
            && !lines[start - 2].trim_start().starts_with("#!")
        {
            start -= 1;
        }
        let name = s(node, "name");
        let qualname = s(&node["metadata"], "qualname");
        let key = (
            s(node, "kind").into(),
            if qualname.is_empty() { name } else { qualname }.into(),
        );
        if ranges.insert(key, json!({"id":node["id"],"language":node["language"],"name":name,"path":path,"start":start,"definition_line":definition,"end":end})).is_some() { return None; }
    }
    Some(ranges)
}
fn inside(rows: &[Value], case: &Value) -> bool {
    !rows.is_empty()
        && rows.iter().all(|row| {
            row[0]
                .as_u64()
                .is_some_and(|line| (n(case, "start")..=n(case, "end")).contains(&line))
        })
}
fn empty_inside(point: u64, case: &Value, others: &[Value], other: &Value) -> bool {
    (n(case, "start")..=n(case, "end")).contains(&point)
        || (point + 1 == n(case, "definition_line")
            && !others.is_empty()
            && others.iter().all(|row| {
                row[1]
                    .as_str()
                    .is_some_and(|line| line.trim_start().starts_with('#'))
            })
            && others
                .iter()
                .filter_map(|row| row[0].as_u64())
                .max()
                .is_some_and(|last| last + 1 == n(other, "definition_line")))
}
pub fn changed(
    graph: &Graph,
    path: &str,
    hunks: &[Value],
    old: &str,
    new: &str,
    helper: Option<&Path>,
    cancel: &AtomicBool,
) -> Option<Vec<Value>> {
    if hunks.is_empty()
        || !graph
            .nodes
            .values()
            .any(|node| s(node, "path") == path && s(node, "kind") == "test")
    {
        return None;
    }
    let nodes: Vec<_> = graph.nodes.values().cloned().collect();
    let after = ranges(&nodes, path, new)?;
    let before = ranges(
        &build::revision_nodes(path, old, helper, cancel).ok()?,
        path,
        old,
    )?;
    if before.is_empty() || after.is_empty() {
        return None;
    }
    let pattern = regex::Regex::new(r"^\s*(?:(?:async\s+)?def\s+(test_[A-Za-z0-9_]*)\s*\(|function\s+(test_[A-Za-z0-9_]*)\b|(test_[A-Za-z0-9_]*)\s*\(\s*\)\s*(?:\{|$))").unwrap();
    let mut selected = BTreeSet::new();
    for hunk in hunks {
        let old_rows = array(hunk, "old_lines");
        let new_rows = array(hunk, "new_lines");
        let definitions = |rows: &[Value]| -> BTreeSet<String> {
            rows.iter()
                .filter_map(|row| pattern.captures(row[1].as_str().unwrap_or("")))
                .filter_map(|c| (1..=3).find_map(|i| c.get(i).map(|m| m.as_str().to_owned())))
                .collect()
        };
        if definitions(old_rows) != definitions(new_rows)
            || n(hunk, "old_count") != old_rows.len() as u64
            || n(hunk, "new_count") != new_rows.len() as u64
        {
            return None;
        }
        let matches: Vec<_> = after
            .iter()
            .filter(|(key, new_case)| {
                before.get(*key).is_some_and(|old_case| {
                    (if old_rows.is_empty() {
                        empty_inside(n(hunk, "old_start"), old_case, new_rows, new_case)
                    } else {
                        inside(old_rows, old_case)
                    }) && (if new_rows.is_empty() {
                        empty_inside(n(hunk, "new_start"), new_case, old_rows, old_case)
                    } else {
                        inside(new_rows, new_case)
                    })
                })
            })
            .collect();
        if matches.len() != 1 {
            return None;
        }
        selected.insert(s(matches[0].1, "id").to_owned());
    }
    Some(after.into_values().filter(|case|selected.contains(s(case,"id"))).map(|case|json!({"id":case["id"],"language":case["language"],"name":case["name"],"path":case["path"]})).collect())
}
pub fn seeds(graph: &Graph, path: &str, cases: &[Value]) -> Option<BTreeSet<String>> {
    if cases.is_empty() {
        return None;
    }
    let mut seeds = BTreeSet::new();
    for case in cases {
        let id = s(case, "id");
        let node = graph.nodes.get(id)?;
        if s(node, "path") != path || !["function", "method"].contains(&s(node, "kind")) {
            return None;
        }
        let mut state = BTreeMap::new();
        let mut stack = vec![(id, false)];
        let mut root = false;
        while let Some((id, exiting)) = stack.pop() {
            if exiting {
                state.insert(id, 2);
                seeds.insert(id.to_owned());
                continue;
            }
            match state.get(id) {
                Some(1) => return None,
                Some(2) => continue,
                _ => {}
            }
            let node = graph.nodes.get(id)?;
            if s(node, "path") != path {
                return None;
            }
            let parents: Vec<_> = graph
                .ins
                .get(id)
                .into_iter()
                .flatten()
                .filter(|edge| s(edge, "relation") == "contains")
                .collect();
            if parents.is_empty() {
                if !["test", "file", "module"].contains(&s(node, "kind")) {
                    return None;
                }
                state.insert(id, 2);
                seeds.insert(id.to_owned());
                root = true;
            } else {
                state.insert(id, 1);
                stack.push((id, true));
                for edge in parents {
                    let parent = s(edge, "source");
                    if s(graph.nodes.get(parent)?, "path") != path || state.get(parent) == Some(&1)
                    {
                        return None;
                    }
                    if state.get(parent) != Some(&2) {
                        stack.push((parent, false));
                    }
                }
            }
        }
        if !root {
            return None;
        }
    }
    Some(seeds)
}
fn shell_ranges(text: &str) -> Option<BTreeMap<String, (usize, usize, usize)>> {
    let definition = regex::Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\s*\(\)\s*\{(.*)$").unwrap();
    let heredoc =
        regex::Regex::new(r#"<<(-?)\s*\\?(?:'([^']+)'|"([^"]+)"|([A-Za-z_][A-Za-z0-9_]*))"#)
            .unwrap();
    let close = regex::Regex::new(r"^}\s*(?:#.*)?$").unwrap();
    let manifest = regex::Regex::new(r"^test_[A-Za-z0-9_]+\(\) \{").unwrap();
    let lines: Vec<_> = text.split('\n').collect();
    let mut pending = std::collections::VecDeque::<(String, bool)>::new();
    let mut current: Option<(String, usize)> = None;
    let mut ranges = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        let line = line.trim_end_matches('\r');
        let number = index + 1;
        if let Some((word, strip)) = pending.front() {
            if (if *strip {
                line.trim_start_matches('\t')
            } else {
                line
            }) == word
            {
                pending.pop_front();
            }
            continue;
        }
        let docs: Vec<_> = heredoc
            .captures_iter(line)
            .filter(|c| {
                c.get(0)
                    .is_some_and(|m| m.start() == 0 || line.as_bytes()[m.start() - 1] != b'<')
            })
            .map(|c| {
                (
                    (2..=4)
                        .find_map(|i| c.get(i).map(|m| m.as_str().to_owned()))
                        .unwrap(),
                    &c[1] == "-",
                )
            })
            .collect();
        if current.is_none() {
            if let Some(c) = definition.captures(line) {
                let name = c[1].to_owned();
                if c[2].trim_end().ends_with('}') && docs.is_empty() {
                    if ranges.insert(name, (number, number, number)).is_some() {
                        return None;
                    }
                    continue;
                }
                current = Some((name, number));
            }
        } else if close.is_match(line) {
            let (name, start) = current.take()?;
            if ranges.insert(name, (start, start, number)).is_some() {
                return None;
            }
            continue;
        }
        pending.extend(docs);
    }
    if current.is_some() || !pending.is_empty() {
        return None;
    }
    for (first, definition, _) in ranges.values_mut() {
        *first = *definition;
        while *first > 1
            && lines[*first - 2].trim_start().starts_with('#')
            && !lines[*first - 2].starts_with("#!")
        {
            *first -= 1;
        }
    }
    let mut ordered: Vec<_> = ranges.values().collect();
    ordered.sort();
    if ordered.windows(2).any(|pair| pair[1].0 <= pair[0].2) {
        return None;
    }
    let manifest: Vec<_> = lines
        .iter()
        .filter(|line| manifest.is_match(line))
        .map(|line| line.split('(').next().unwrap())
        .collect();
    let mut mine: Vec<_> = ranges
        .iter()
        .filter(|(name, _)| name.starts_with("test_"))
        .map(|(name, range)| (range.1, name.as_str()))
        .collect();
    mine.sort();
    if mine.iter().map(|(_, name)| *name).collect::<Vec<_>>() != manifest {
        return None;
    }
    Some(ranges)
}
pub fn suite(old: &str, new: &str, hunks: &[Value]) -> Option<Vec<String>> {
    if hunks.is_empty() {
        return None;
    }
    let before = shell_ranges(old)?;
    let after = shell_ranges(new)?;
    let shared =
        regex::Regex::new(r"^\s*#\s*(?:!|shellcheck|SMOKE_TEST_CALLS|smoke-weight)").unwrap();
    let mut selected = BTreeSet::new();
    for hunk in hunks {
        for (side, ranges) in [("old_lines", &before), ("new_lines", &after)] {
            for row in array(hunk, side) {
                let number = row[0].as_u64()? as usize;
                let line = row[1].as_str()?;
                let owner = ranges
                    .iter()
                    .find(|(_, range)| range.0 <= number && number <= range.2)
                    .map(|(name, _)| name);
                match owner {
                    None if line.trim().is_empty()
                        || (line.trim_start().starts_with('#') && !shared.is_match(line)) => {}
                    Some(name) if name.starts_with("test_") => {
                        if after.contains_key(name) {
                            selected.insert(name.clone());
                        }
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(selected.into_iter().collect())
}
pub fn suite_rows(path: &str, names: &[String]) -> Vec<Value> {
    names.iter().map(|name|json!({"id":format!("symbol:{path}::{name}"),"language":"shell","name":name,"path":path})).collect()
}
