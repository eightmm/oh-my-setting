use super::*;
use std::collections::BTreeMap;
fn prefix(pattern: &str) -> &str {
    let end = pattern.find(['*', '?', '[']).unwrap_or(pattern.len());
    pattern[..end]
        .rsplit_once('/')
        .map(|(p, _)| p)
        .unwrap_or("")
}
fn literal(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}
pub fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return true;
    }
    for a in left {
        for b in right {
            if !util::safe_path(a) || !util::safe_path(b) {
                return true;
            }
            let ag = a.contains(['*', '?', '[']);
            let bg = b.contains(['*', '?', '[']);
            let overlap = match (ag, bg) {
                (false, false) => literal(a, b),
                (true, true) => {
                    let (a, b) = (prefix(a), prefix(b));
                    a.is_empty() || b.is_empty() || literal(a, b)
                }
                _ => {
                    let (l, g) = if ag { (b, a) } else { (a, b) };
                    util::glob(g, l) || prefix(g).is_empty() || literal(l, prefix(g))
                }
            };
            if overlap {
                return true;
            }
        }
    }
    false
}
#[allow(clippy::too_many_arguments)]
pub fn eligible(
    spec: &Value,
    facts: &Value,
    route: &Value,
    scopes: &Value,
    capacity: i64,
    active: &[String],
    resolved: &Value,
    selectors: &[String],
) -> Value {
    let mut candidates = vec![];
    for item in std::iter::once(route.get("primary")).chain(ar(route, "alternatives")) {
        let name = item.text().unwrap_or("");
        if !name.is_empty() && !candidates.iter().any(|n| n == name) {
            candidates.push(name.to_owned());
        }
    }
    let mut eligible = vec![];
    let mut deferred = vec![];
    let mut conflicts = vec![];
    if st(route, "status") != "actionable" {
        for name in if candidates.is_empty() {
            vec![String::new()]
        } else {
            candidates
        } {
            deferred.push(obj([
                ("node", name.into()),
                (
                    "reason",
                    format!("route:{}", route.get("status").or("unknown").py()).into(),
                ),
            ]));
        }
        return obj([
            ("eligible", list(eligible)),
            ("deferred", list(deferred)),
            ("conflicts", list(conflicts)),
        ]);
    }
    let task = |id: &str, node: &Value| -> String {
        if resolved.get(id).truth() {
            resolved.get(id).py()
        } else if st(node, "plan_task") != "next" {
            st(node, "plan_task").into()
        } else {
            String::new()
        }
    };
    let scope = |node: &Value, task: &str| -> Option<Vec<String>> {
        if node.default("effect", "read") != "write".into() {
            return Some(vec![]);
        }
        if st(node, "kind") != "agent" {
            return None;
        }
        let allowed = scopes.get(task).get("allowed");
        if !matches!(allowed, Value::Arr(_)) || allowed.array().is_empty() {
            None
        } else {
            Some(allowed.array().iter().map(Value::py).collect())
        }
    };
    let mut held = BTreeMap::<String, Option<Vec<String>>>::new();
    let mut held_tasks = BTreeMap::<String, String>::new();
    for name in active {
        let node = spec.get("nodes").get(name);
        if !matches!(node, Value::Obj(_)) {
            held.insert(name.clone(), None);
            continue;
        }
        let task = task(name, node);
        if !task.is_empty() {
            held_tasks.insert(task.clone(), name.clone());
        }
        if node.default("effect", "read") == "write".into() {
            held.insert(name.clone(), scope(node, &task));
        }
    }
    let mut selected_scopes = BTreeMap::<String, Option<Vec<String>>>::new();
    let mut selected_tasks = BTreeMap::<String, String>::new();
    let mut exclusive_selected = false;
    let mut selector_selected = active.iter().any(|n| selectors.contains(n));
    for name in candidates {
        let node = spec.get("nodes").get(&name);
        let absent = missing(ar(node, "requires"), facts);
        let deferred_reason = if !matches!(node, Value::Obj(_)) {
            Some("unknown-node".into())
        } else if st(node, "kind") == "gate" {
            Some("gate".into())
        } else if let Some(absent) = absent.first() {
            Some(format!("requires:{}", absent.py()))
        } else if exclusive_selected {
            Some("exclusive".into())
        } else {
            None
        };
        if let Some(reason) = deferred_reason {
            deferred.push(obj([("node", name.into()), ("reason", reason.into())]));
            continue;
        }
        let task = task(&name, node);
        let agent = st(node, "kind") == "agent";
        if agent
            && !task.is_empty()
            && let Some(other) = held_tasks.get(&task).or_else(|| selected_tasks.get(&task))
        {
            conflicts.push(obj([
                ("node", name.into()),
                ("with", other.clone().into()),
                ("reason", "same-task".into()),
            ]));
            continue;
        }
        if selectors.contains(&name) && selector_selected {
            deferred.push(obj([
                ("node", name.into()),
                ("reason", "dynamic-selector-exclusive".into()),
            ]));
            continue;
        }
        let write = node.default("effect", "read") == "write".into();
        let scope = scope(node, &task);
        let exclusive = agent && node.default("mode", "run") == "land".into()
            || st(node, "kind") == "tool" && write;
        if write && scope.is_none() && agent {
            conflicts.push(obj([
                ("node", name.into()),
                ("with", "".into()),
                ("reason", "unknown-scope".into()),
            ]));
            continue;
        }
        if exclusive && (!active.is_empty() || !eligible.is_empty()) {
            deferred.push(obj([("node", name.into()), ("reason", "exclusive".into())]));
            continue;
        }
        if write && let Some(ref scope) = scope {
            let blocker = held
                .iter()
                .chain(selected_scopes.iter())
                .find(|(_, other)| scopes_overlap(scope, other.as_deref().unwrap_or(&[])));
            if let Some((other, _)) = blocker {
                conflicts.push(obj([
                    ("node", name.into()),
                    ("with", other.clone().into()),
                    ("reason", "scope-overlap".into()),
                ]));
                continue;
            }
        }
        if eligible.len() >= capacity.max(1) as usize {
            deferred.push(obj([("node", name.into()), ("reason", "capacity".into())]));
            continue;
        }
        eligible.push(Value::from(name.clone()));
        if agent && !task.is_empty() {
            selected_tasks.insert(task, name.clone());
        }
        if selectors.contains(&name) {
            selector_selected = true;
        }
        if write {
            selected_scopes.insert(name, scope);
        }
        if exclusive {
            exclusive_selected = true;
        }
    }
    obj([
        ("eligible", list(eligible)),
        ("deferred", list(deferred)),
        ("conflicts", list(conflicts)),
    ])
}
