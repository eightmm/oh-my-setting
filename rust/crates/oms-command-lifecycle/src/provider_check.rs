use crate::{
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

pub(crate) fn check(table: &Value, paths: &Paths) -> Result<Value> {
    let functions = table["functions"]
        .as_array()
        .ok_or_else(|| r::Error(2, "provider functions must be an array".into()))?;
    let providers = table["providers"]
        .as_object()
        .ok_or_else(|| r::Error(2, "provider definitions must be an object".into()))?;
    let mut problems = Vec::<String>::new();
    let mut cache = BTreeMap::<String, Option<String>>::new();
    for (provider, spec) in providers {
        let mut cells = match spec.get("profile") {
            None => serde_json::Map::new(),
            Some(profile) => table["profiles"]
                .get(profile.as_str().unwrap_or(""))
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| r::Error(2, "unknown provider capability profile".into()))?,
        };
        if let Some(overrides) = spec["overrides"].as_object() {
            for (key, cell) in overrides {
                let cell = if let Some(name) = cell["$ref"].as_str() {
                    table["refs"].get(name).ok_or_else(|| {
                        r::Error(2, "unknown provider capability reference".into())
                    })?
                } else {
                    cell
                };
                cells.insert(key.clone(), cell.clone());
            }
        }
        for function in functions {
            let name = function["id"]
                .as_str()
                .ok_or_else(|| r::Error(2, "invalid provider function".into()))?;
            let cell = spec
                .get(name)
                .or_else(|| cells.get(name))
                .unwrap_or(&Value::Null);
            let label = format!("{provider}/{name}");
            if !matches!(
                cell["status"].as_str(),
                Some("supported" | "partial" | "unsupported" | "unknown")
            ) {
                problems.push(format!("{label}: invalid status"));
                continue;
            }
            if !matches!(cell["verified"].as_str(), Some("fixture" | "live")) {
                problems.push(format!("{label}: verified must be fixture or live"));
            }
            if !matches!(cell["status"].as_str(), Some("supported" | "partial")) {
                continue;
            }
            if cell["status"] == "partial" && cell["limit"].as_str().is_none_or(str::is_empty) {
                problems.push(format!("{label}: partial needs a limit"));
            }
            for kind in ["impl", "test"] {
                let Some(entries) = cell[kind].as_array().filter(|a| !a.is_empty()) else {
                    problems.push(format!("{label}: missing {kind} evidence"));
                    continue;
                };
                for entry in entries {
                    let Some((file, word)) = entry
                        .as_str()
                        .and_then(|s| s.split_once("::"))
                        .filter(|(_, w)| !w.is_empty())
                    else {
                        problems.push(format!("{label}: invalid {kind} reference"));
                        continue;
                    };
                    let path = Path::new(file);
                    if path.is_absolute()
                        || path
                            .components()
                            .any(|c| !matches!(c, Component::Normal(_)))
                    {
                        problems.push(format!("{label}: unsafe {kind} reference"));
                        continue;
                    }
                    let text = cache.entry(file.into()).or_insert_with(|| {
                        let full = paths.source.join(path);
                        if !r::physical(&full).is_ok_and(|p| p.starts_with(&paths.source)) {
                            return None;
                        }
                        String::from_utf8(r::read(&full, r::LIMIT).ok()?).ok()
                    });
                    let symbol = regex::escape(word);
                    let pattern = if kind == "impl" && file.ends_with(".py") {
                        format!(
                            r"(?m)^\s*(?:async\s+)?(?:def|class)\s+{symbol}\b|(?m)^{symbol}\s*="
                        )
                    } else if kind == "impl" && file.ends_with(".sh") {
                        format!(r"(?m)^\s*(?:function\s+)?{symbol}\s*\(\)")
                    } else {
                        format!(r"(?:^|[^\w-]){symbol}(?:$|[^\w-])")
                    };
                    if !text
                        .as_ref()
                        .is_some_and(|t| regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(t)))
                    {
                        problems.push(format!(
                            "{label}: {kind} reference not found: {file}::{word}"
                        ));
                    }
                }
            }
        }
    }
    Ok(json!({"schema":1,"checked":providers.len(),"problems":problems,"ok":problems.is_empty()}))
}
