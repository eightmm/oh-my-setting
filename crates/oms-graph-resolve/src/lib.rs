//! Pure cross-file resolution for parser records. No filesystem access is performed.
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// A parser result together with the extraction metadata supplied by its caller.
#[derive(Clone, Debug)]
pub struct Extraction {
    pub path: String,
    pub sha256: String,
    pub parser: String,
    pub nodes: Vec<Value>,
    pub edges: Vec<Value>,
    pub refs: Vec<Value>,
}

/// Invalid records are rejected before any resolution; parser fields must have
/// their documented JSON types and lines must be nonnegative integer numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn id(kind: &str, path: &str, name: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches('/');
    match kind {
        "env" => format!("env:{}", if name.is_empty() { path } else { name }),
        "class" | "function" | "method" | "symbol" => format!("symbol:{path}::{name}"),
        _ => format!("{kind}:{path}"),
    }
}
fn validate(items: &[Extraction]) -> Result<(), Error> {
    for item in items {
        for (label, rows) in [
            ("node", &item.nodes),
            ("edge", &item.edges),
            ("ref", &item.refs),
        ] {
            for row in rows {
                let obj = row
                    .as_object()
                    .ok_or_else(|| Error(format!("{label} must be an object")))?;
                let required: &[&str] = match label {
                    "node" => &["id", "kind", "name", "language"],
                    "edge" => &["source", "target", "relation", "confidence"],
                    _ => &["kind", "value"],
                };
                for key in required {
                    if !obj.get(*key).is_some_and(Value::is_string) {
                        return Err(Error(format!("{label}.{key} must be a string")));
                    }
                }
                if label == "node"
                    && s(row, "kind") != "env"
                    && !obj.get("path").is_some_and(Value::is_string)
                {
                    return Err(Error("node.path must be a string".into()));
                }
                for key in ["from", "relation", "binding", "imported"] {
                    if label == "ref" && obj.get(key).is_some_and(|v| !v.is_string()) {
                        return Err(Error(format!("ref.{key} must be a string")));
                    }
                }
                for key in [
                    "scoped",
                    "method_call",
                    "shell_bare",
                    "variable",
                    "heredoc_python",
                ] {
                    if label == "ref" && obj.get(key).is_some_and(|v| !v.is_boolean()) {
                        return Err(Error(format!("ref.{key} must be a boolean")));
                    }
                }
                let line = if label == "edge" {
                    match obj.get("evidence") {
                        Some(v) if v.is_object() => v.get("line"),
                        None => None,
                        _ => return Err(Error("edge.evidence must be an object".into())),
                    }
                } else {
                    obj.get("line")
                };
                if line.is_some_and(|v| !v.is_null() && v.as_u64().is_none()) {
                    return Err(Error(format!(
                        "{label}.line must be a nonnegative integer or null"
                    )));
                }
                if label == "edge"
                    && row
                        .get("evidence")
                        .and_then(|v| v.get("path"))
                        .is_some_and(|v| !v.is_string() && !v.is_null())
                {
                    return Err(Error("edge.evidence.path must be a string or null".into()));
                }
            }
        }
    }
    Ok(())
}
fn module_path(
    value: &str,
    source: &str,
    files: &HashMap<&str, usize>,
    modules: &HashMap<String, String>,
) -> String {
    if value.starts_with('.') {
        let level = value.bytes().take_while(|b| *b == b'.').count();
        let mut parts: Vec<&str> = source
            .rsplit_once('/')
            .map_or(Vec::new(), |(p, _)| p.split('/').collect());
        for _ in 1..level {
            parts.pop();
        }
        let rest = &value[level..];
        if !rest.is_empty() {
            parts.extend(rest.split('.'));
        }
        let base = parts.join("/");
        for suffix in [".py", "/__init__.py"] {
            let p = format!("{base}{suffix}");
            if files.contains_key(p.as_str()) {
                return p;
            }
        }
        return String::new();
    }
    if let Some(p) = modules.get(value) {
        return p.clone();
    }
    let base = value.replace('.', "/");
    let choices = [format!("{base}.py"), format!("{base}/__init__.py")];
    for p in &choices {
        if files.contains_key(p.as_str()) {
            return p.clone();
        }
    }
    if let Some((dir, _)) = source.rsplit_once('/') {
        for p in &choices {
            let p = format!("{dir}/{p}");
            if files.contains_key(p.as_str()) {
                return p;
            }
        }
    }
    String::new()
}
fn submodule(
    value: &str,
    name: &str,
    source: &str,
    files: &HashMap<&str, usize>,
    modules: &HashMap<String, String>,
) -> String {
    if name.is_empty() {
        return String::new();
    }
    module_path(
        &format!(
            "{value}{}{name}",
            if value.ends_with('.') { "" } else { "." }
        ),
        source,
        files,
        modules,
    )
}
fn path_target(value: &str, files: &HashMap<&str, usize>) -> String {
    if files.contains_key(value) {
        return value.to_owned();
    }
    for prefix in ["", "scripts/", "scripts/lib/"] {
        let p = format!("{prefix}{value}");
        if files.contains_key(p.as_str()) {
            return p;
        }
    }
    value.to_owned()
}
fn edge(
    source: &str,
    target: &str,
    relation: &str,
    confidence: &str,
    item: &Extraction,
    reference: &Value,
    count: usize,
) -> Value {
    let mut evidence = json!({"path": item.path.replace('\\', "/"), "source_digest": item.sha256});
    if let Some(line) = reference.get("line").filter(|v| !v.is_null()) {
        evidence["line"] = line.clone();
    }
    if count > 1 {
        evidence["candidate_count"] = json!(count);
    }
    json!({"source":source,"target":target,"relation":relation,"confidence":confidence,"evidence":evidence})
}
type EdgeKey = (String, String, String, String, Option<String>, Option<u64>);
fn edge_key(v: &Value) -> EdgeKey {
    let ev = &v["evidence"];
    (
        s(v, "source").into(),
        s(v, "target").into(),
        s(v, "relation").into(),
        s(v, "confidence").into(),
        ev.get("path").and_then(Value::as_str).map(str::to_owned),
        ev.get("line").and_then(Value::as_u64),
    )
}
/// Resolve ordered extraction records into sorted graph nodes and edges.
/// Duplicate paths retain their first iteration position and last file record;
/// duplicate nodes and edges follow the legacy overwrite rules.
pub fn resolve(items: &[Extraction]) -> Result<(Vec<Value>, Vec<Value>), Error> {
    validate(items)?;
    let mut node_map = BTreeMap::new();
    let mut raw = Vec::new();
    let mut files = HashMap::new();
    let mut file_order = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if files.insert(item.path.as_str(), i).is_none() {
            file_order.push(item.path.as_str());
        }
        for n in &item.nodes {
            if s(n, "kind") == "env" {
                node_map
                    .entry(s(n, "id").to_owned())
                    .or_insert_with(|| n.clone());
            } else {
                node_map.insert(s(n, "id").to_owned(), n.clone());
            }
        }
        raw.extend(item.edges.iter().cloned());
    }
    let nodes: Vec<Value> = node_map.into_values().collect();
    let by_id: HashMap<&str, &Value> = nodes.iter().map(|n| (s(n, "id"), n)).collect();
    let mut names: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut symbols: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for n in &nodes {
        if ["class", "function", "method", "symbol"].contains(&s(n, "kind")) {
            names.entry(s(n, "name")).or_default().push(s(n, "id"));
            symbols
                .entry(s(n, "path"))
                .or_default()
                .insert(s(n, "name"), s(n, "id"));
        }
    }
    let mut modules = HashMap::new();
    for path in file_order {
        if path.ends_with(".py") {
            for prefix in ["", "src/", "scripts/", "scripts/lib/"] {
                if let Some(trimmed) = path.strip_prefix(prefix) {
                    let mut module = trimmed[..trimmed.len() - 3].replace('/', ".");
                    if module.ends_with(".__init__") {
                        module.truncate(module.len() - 9);
                    }
                    modules.entry(module).or_insert_with(|| path.to_owned());
                }
            }
        }
    }
    let mut imports: HashMap<&str, HashMap<&str, (String, String)>> = HashMap::new();
    let mut includes: HashMap<&str, Vec<String>> = HashMap::new();
    for item in items {
        for r in &item.refs {
            if s(r, "relation") != "imports" || flag(r, "heredoc_python") {
                continue;
            }
            let mut imported = s(r, "imported").to_owned();
            let mut p = if s(r, "kind") == "module" {
                let sub = submodule(s(r, "value"), &imported, &item.path, &files, &modules);
                if sub.is_empty() {
                    module_path(s(r, "value"), &item.path, &files, &modules)
                } else {
                    imported.clear();
                    sub
                }
            } else {
                s(r, "value").to_owned()
            };
            if s(r, "kind") == "path" {
                p = path_target(&p, &files);
            }
            if files.contains_key(p.as_str()) {
                imports
                    .entry(&item.path)
                    .or_default()
                    .insert(s(r, "binding"), (p.clone(), imported));
                includes.entry(&item.path).or_default().push(p);
            }
        }
    }
    for item in items {
        let file_id = id(
            if item
                .nodes
                .iter()
                .any(|n| s(n, "id") == id("test", &item.path, ""))
            {
                "test"
            } else {
                "file"
            },
            &item.path,
            "",
        );
        for r in &item.refs {
            let value = s(r, "value");
            let mut relation = r
                .get("relation")
                .and_then(Value::as_str)
                .unwrap_or("references");
            let mut confidence = "EXTRACTED";
            let mut targets: Vec<&str> = Vec::new();
            let mut owned = String::new();
            match s(r, "kind") {
                "module" => {
                    let sub = submodule(value, s(r, "imported"), &item.path, &files, &modules);
                    let p = if sub.is_empty() {
                        module_path(value, &item.path, &files, &modules)
                    } else {
                        sub
                    };
                    if !p.is_empty() {
                        let m = id("module", &p, "");
                        owned = if by_id.contains_key(m.as_str()) {
                            m
                        } else {
                            id("file", &p, "")
                        };
                        if file_id.starts_with("test:") {
                            relation = "tests";
                        }
                    }
                }
                "path" => {
                    let p = path_target(value, &files);
                    if files.contains_key(p.as_str()) {
                        owned = id(
                            if by_id.contains_key(id("test", &p, "").as_str()) {
                                "test"
                            } else {
                                "file"
                            },
                            &p,
                            "",
                        );
                        if flag(r, "variable") {
                            confidence = "INFERRED";
                        }
                    }
                }
                "name" => {
                    let (first, member) = value.split_once('.').unwrap_or((value, ""));
                    if let Some((p, imported)) =
                        imports.get(item.path.as_str()).and_then(|m| m.get(first))
                    {
                        if !member.is_empty() && !imported.is_empty() {
                            let m = id("method", p, &format!("{imported}.{member}"));
                            if by_id.contains_key(m.as_str()) {
                                owned = m;
                            }
                        }
                        if owned.is_empty()
                            && let Some(t) = symbols.get(p.as_str()).and_then(|m| {
                                m.get(if member.is_empty() {
                                    imported.as_str()
                                } else {
                                    member
                                })
                            })
                        {
                            targets.push(t);
                        }
                    } else {
                        if !member.is_empty() {
                            let m = id("method", &item.path, value);
                            if by_id.contains_key(m.as_str()) {
                                owned = m;
                            }
                        }
                        if owned.is_empty()
                            && let Some(t) =
                                symbols.get(item.path.as_str()).and_then(|m| m.get(first))
                        {
                            targets.push(t);
                        }
                        if owned.is_empty()
                            && targets.is_empty()
                            && flag(r, "shell_bare")
                            && let Some(paths) = includes.get(item.path.as_str())
                        {
                            for p in paths {
                                if let Some(t) = symbols.get(p.as_str()).and_then(|m| m.get(first))
                                {
                                    targets.push(t);
                                    confidence = "INFERRED";
                                    break;
                                }
                            }
                        }
                        if owned.is_empty()
                            && targets.is_empty()
                            && member.is_empty()
                            && !flag(r, "scoped")
                            && let Some(choices) = names.get(first)
                        {
                            targets.extend(choices.iter().copied().filter(|c| {
                                let n = by_id[c];
                                if flag(r, "shell_bare") {
                                    s(n, "language") == "shell" && s(n, "kind") == "function"
                                } else if item.parser == "python" || item.parser == "notebook" {
                                    s(n, "language") == "python"
                                        && (s(n, "kind") != "method" || flag(r, "method_call"))
                                } else {
                                    true
                                }
                            }));
                            if targets.len() == 1 {
                                confidence = "INFERRED";
                            } else if targets.len() > 1 {
                                confidence = "AMBIGUOUS";
                            }
                        }
                    }
                }
                _ => {}
            }
            if !owned.is_empty() {
                targets.push(&owned);
            }
            let count = targets.len();
            for target in targets {
                raw.push(edge(
                    r.get("from").and_then(Value::as_str).unwrap_or(&file_id),
                    target,
                    relation,
                    confidence,
                    item,
                    r,
                    count,
                ));
            }
        }
    }
    // Index preserves the original insertion position when a duplicate is replaced.
    let mut unique: HashMap<EdgeKey, usize> = HashMap::new();
    let mut edges = Vec::new();
    for e in raw {
        let key = edge_key(&e);
        if let Some(&i) = unique.get(&key) {
            edges[i] = e;
        } else {
            unique.insert(key, edges.len());
            edges.push(e);
        }
    }
    edges.sort_by(|a, b| {
        fn key(v: &Value) -> (&str, &str, &str, &str, u64) {
            (
                s(v, "source"),
                s(v, "target"),
                s(v, "relation"),
                s(v, "confidence"),
                v["evidence"]
                    .get("line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        }
        key(a).cmp(&key(b))
    });
    Ok((nodes, edges))
}
