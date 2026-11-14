//! Ownership and convergence of Claude settings hook rows. Arbitrary wrappers
//! and same-named scripts outside a verified OMS root remain user-owned.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
const SCRIPTS: &[&str] = &[
    "skill-router.sh",
    "turn-guard.sh",
    "fail-ledger-hook.sh",
    "syntax-guard-hook.sh",
    "tier-guard-hook.sh",
    "precompact-handoff.sh",
    "resume-hook.sh",
    "telemetry-hook.sh",
    "git-move-guard-hook.sh",
    "panel-direct-work-hook.sh",
    "command-guard-hook.sh",
];
fn norm(s: &str) -> String {
    s.replace('\\', "/").trim_end_matches('/').into()
}
pub fn entry_command(root: &Path, executable: &str, path: &Path) -> Result<String, String> {
    [
        "bash".to_owned(),
        root.join("scripts/lib/python-entry.sh")
            .to_string_lossy()
            .into_owned(),
        executable.into(),
        path.to_string_lossy().into_owned(),
    ]
    .iter()
    .map(|s| {
        shlex::try_quote(s)
            .map(|s| s.into_owned())
            .map_err(|_| "invalid command path".into())
    })
    .collect::<Result<Vec<_>, _>>()
    .map(|s| s.join(" "))
}
pub fn hook_command(root: &Path, script: &str) -> Result<String, String> {
    entry_command(root, "bash", &root.join("scripts").join(script))
}
pub fn entry_path(command: &str, executable: &str) -> Option<PathBuf> {
    let argv = shlex::split(command)?;
    if argv.len() == 2 && argv[0] == executable {
        return Some((&argv[1]).into());
    }
    if argv.len() == 4 && argv[0] == "bash" && argv[2] == executable {
        let path = norm(&argv[3]);
        let root = path.rsplitn(3, '/').collect::<Vec<_>>();
        if root.len() == 3 && norm(&argv[1]) == format!("{}/scripts/lib/python-entry.sh", root[2]) {
            return Some((&argv[3]).into());
        }
    }
    None
}
pub fn is_oms_root(candidate: &Path, roots: &[PathBuf]) -> bool {
    if roots
        .iter()
        .any(|p| norm(&p.to_string_lossy()) == norm(&candidate.to_string_lossy()))
    {
        return true;
    }
    let Ok(real) = candidate.canonicalize() else {
        return false;
    };
    roots
        .iter()
        .any(|p| p.canonicalize().ok().as_ref() == Some(&real))
        || real.join(".agents/plugins/marketplace.json").is_file()
            && real.join("scripts/install-claude-hooks.sh").is_file()
}
fn script_path(command: &str) -> Option<PathBuf> {
    entry_path(command, "bash").or_else(|| {
        let rest = command.strip_prefix("bash ")?.trim();
        (!rest.is_empty() && !rest.chars().any(|c| "'\"\\$`;&|<>".contains(c))).then(|| rest.into())
    })
}
fn dead(command: &str, root: &Path, roots: &[PathBuf]) -> bool {
    let Some(argv) = shlex::split(command) else {
        return false;
    };
    argv.len() == 4
        && argv[0] == "bash"
        && argv[2] == "bash"
        && norm(&argv[1]) == norm(&root.join("scripts/lib/python-entry.sh").to_string_lossy())
        && !is_oms_root(root, roots)
        && !root.join("scripts/lib/python-entry.sh").exists()
}
pub fn owned_script(hook: &Value, roots: &[PathBuf]) -> Option<String> {
    let obj = hook.as_object()?;
    if obj.get("type").is_some_and(|t| t != "command") {
        return None;
    }
    let command = obj.get("command")?.as_str()?;
    let path = script_path(command)?;
    let n = norm(&path.to_string_lossy());
    let parts = n.rsplitn(3, '/').collect::<Vec<_>>();
    if parts.len() != 3 || parts[1] != "scripts" || !SCRIPTS.contains(&parts[0]) {
        return None;
    }
    let root = Path::new(parts[2]);
    if !root.is_absolute() && !(parts[2].len() > 2 && parts[2].as_bytes().get(1..3) == Some(b":/"))
    {
        return None;
    }
    (is_oms_root(root, roots) || dead(command, root, roots)).then(|| parts[0].into())
}
fn matcher_ok(entry: &Value, matcher: &Value) -> bool {
    if matcher.is_null() {
        entry.get("matcher").is_none_or(|m| m.is_null() || m == "")
    } else {
        entry.get("matcher") == Some(matcher)
    }
}
fn desired(hook: &Value, surface: &Value, root: &Path) -> Result<Value, String> {
    let mut hook = hook.as_object().cloned().unwrap_or_default();
    hook.insert("type".into(), json!("command"));
    let script = surface["script"].as_str().ok_or("surface script missing")?;
    hook.insert("command".into(), json!(hook_command(root, script)?));
    if surface["timeout"].is_null() {
        hook.remove("timeout");
    } else {
        hook.insert("timeout".into(), surface["timeout"].clone());
    }
    if !(surface["fail_closed"] == true && hook.get("onFailure").is_some_and(|v| v == "block")) {
        hook.remove("onFailure");
    }
    Ok(Value::Object(hook))
}
/// Converges owned rows in place, preserving sibling order and entry metadata.
pub fn converge(
    hooks: &mut Value,
    surfaces: &[Value],
    root: &Path,
    roots: &[PathBuf],
) -> Result<(), String> {
    let hooks = hooks.as_object_mut().ok_or("hooks must be an object")?;
    let mut want = BTreeMap::new();
    for row in surfaces {
        let event = row["event"].as_str().ok_or("surface event missing")?;
        let script = row["script"].as_str().ok_or("surface script missing")?;
        want.insert((event.to_owned(), script.to_owned()), row);
    }
    let mut seen = BTreeSet::new();
    for event in hooks.keys().cloned().collect::<Vec<_>>() {
        let Some(entries) = hooks.get(&event).and_then(Value::as_array) else {
            continue;
        };
        let mut result = Vec::new();
        for entry in entries {
            let Some(rows) = entry["hooks"].as_array() else {
                result.push(entry.clone());
                continue;
            };
            let mut segments: Vec<(Option<Value>, Vec<Value>)> = Vec::new();
            let mut changed = false;
            for hook in rows {
                let mut matcher = None;
                let mut item = hook.clone();
                if let Some(script) = owned_script(hook, roots) {
                    let key = (event.clone(), script);
                    let Some(surface) = want.get(&key).filter(|_| !seen.contains(&key)) else {
                        changed = true;
                        continue;
                    };
                    seen.insert(key);
                    item = desired(hook, surface, root)?;
                    changed |= item != *hook;
                    if !matcher_ok(entry, &surface["matcher"]) {
                        matcher = Some(surface["matcher"].clone());
                        changed = true;
                    }
                }
                if let Some((prev, items)) =
                    segments.last_mut().filter(|(prev, _)| *prev == matcher)
                {
                    let _ = prev;
                    items.push(item);
                } else {
                    segments.push((matcher, vec![item]));
                }
            }
            if !changed {
                result.push(entry.clone());
                continue;
            }
            for (matcher, items) in segments {
                let mut entry = entry.clone();
                entry["hooks"] = json!(items);
                if let Some(matcher) = matcher {
                    entry.as_object_mut().unwrap().remove("matcher");
                    if !matcher.is_null() {
                        entry["matcher"] = matcher;
                    }
                }
                result.push(entry);
            }
        }
        if result.is_empty() {
            hooks.remove(&event);
        } else {
            hooks.insert(event, json!(result));
        }
    }
    for row in surfaces {
        let event = row["event"].as_str().unwrap();
        let script = row["script"].as_str().unwrap();
        if seen.contains(&(event.into(), script.into())) {
            continue;
        }
        let mut entry = json!({"hooks":[desired(&json!({}),row,root)?]});
        if !row["matcher"].is_null() {
            entry["matcher"] = row["matcher"].clone();
        }
        let entries = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.{event} is not a list"))?;
        entries.push(entry);
    }
    Ok(())
}
pub fn remove(hooks: &mut Value, roots: &[PathBuf]) {
    let Some(hooks) = hooks.as_object_mut() else {
        return;
    };
    for event in hooks.keys().cloned().collect::<Vec<_>>() {
        let Some(entries) = hooks[&event].as_array() else {
            continue;
        };
        let mut kept = Vec::new();
        for entry in entries {
            let Some(rows) = entry["hooks"].as_array() else {
                kept.push(entry.clone());
                continue;
            };
            let rows_after: Vec<_> = rows
                .iter()
                .filter(|h| owned_script(h, roots).is_none())
                .cloned()
                .collect();
            if rows_after.len() == rows.len() {
                kept.push(entry.clone());
            } else if !rows_after.is_empty() {
                let mut e = entry.clone();
                e["hooks"] = json!(rows_after);
                kept.push(e);
            }
        }
        if kept.is_empty() {
            hooks.remove(&event);
        } else {
            hooks.insert(event, json!(kept));
        }
    }
}
/// Set/clear provider fail-closed policy only on the capable owned surfaces.
pub fn apply_fail_closed(
    hooks: &mut Value,
    surfaces: &[Value],
    roots: &[PathBuf],
    mode: &str,
) -> Vec<(String, String)> {
    let mut changed = Vec::new();
    if !["on", "off"].contains(&mode) {
        return changed;
    }
    let Some(hooks) = hooks.as_object_mut() else {
        return changed;
    };
    for (event, entries) in hooks {
        for entry in entries.as_array_mut().into_iter().flatten() {
            for hook in entry["hooks"].as_array_mut().into_iter().flatten() {
                let Some(script) = owned_script(hook, roots) else {
                    continue;
                };
                if !surfaces.iter().any(|s| {
                    s["event"] == event.as_str()
                        && s["script"] == script
                        && s["fail_closed"] == true
                }) {
                    continue;
                }
                if mode == "on" && hook["onFailure"] != "block" {
                    hook["onFailure"] = json!("block");
                    changed.push((event.clone(), script));
                } else if mode == "off" && hook.get("onFailure").is_some() {
                    hook.as_object_mut().unwrap().remove("onFailure");
                    changed.push((event.clone(), script));
                }
            }
        }
    }
    changed
}
pub fn fail_open(hooks: &Value, surfaces: &[Value], roots: &[PathBuf]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for (event, entries) in hooks.as_object().into_iter().flatten() {
        for entry in entries.as_array().into_iter().flatten() {
            for hook in entry["hooks"].as_array().into_iter().flatten() {
                if let Some(script) = owned_script(hook, roots)
                    && hook["onFailure"] != "block"
                    && surfaces.iter().any(|s| {
                        s["event"] == event.as_str()
                            && s["script"] == script
                            && s["fail_closed"] == true
                    })
                {
                    result.push((event.clone(), script));
                }
            }
        }
    }
    result.sort();
    result
}
pub type AuditProblems = BTreeMap<(String, String), Vec<(String, String)>>;
pub fn audit(
    hooks: &Value,
    surfaces: &[Value],
    root: &Path,
    roots: &[PathBuf],
) -> Result<(AuditProblems, Vec<(String, String)>), String> {
    let mut found = BTreeMap::<(String, String), Vec<(&Value, &Value)>>::new();
    for (event, entries) in hooks.as_object().into_iter().flatten() {
        for entry in entries.as_array().into_iter().flatten() {
            for hook in entry["hooks"].as_array().into_iter().flatten() {
                if let Some(script) = owned_script(hook, roots) {
                    found
                        .entry((event.clone(), script))
                        .or_default()
                        .push((entry, hook));
                }
            }
        }
    }
    let mut problems = BTreeMap::new();
    for row in surfaces {
        let event = row["event"].as_str().ok_or("surface event missing")?;
        let script = row["script"].as_str().ok_or("surface script missing")?;
        let key = (event.into(), script.into());
        let rows = found.remove(&key).unwrap_or_default();
        let mut issues = Vec::new();
        if rows.is_empty() {
            issues.push(("missing".into(), String::new()));
        }
        if rows.len() > 1 {
            issues.push(("duplicate".into(), format!("{} registrations", rows.len())));
        }
        for (entry, hook) in rows {
            let desired = desired(hook, row, root)?;
            for key in ["type", "command", "timeout", "onFailure"] {
                if hook.get(key) != desired.get(key) {
                    issues.push((
                        "drift".into(),
                        format!(
                            "{key} {}, expected {}",
                            hook.get(key).unwrap_or(&Value::Null),
                            desired.get(key).unwrap_or(&Value::Null)
                        ),
                    ));
                }
            }
            if !matcher_ok(entry, &row["matcher"]) {
                issues.push((
                    "drift".into(),
                    format!("matcher {}, expected {}", entry["matcher"], row["matcher"]),
                ));
            }
            if let Some(path) = entry_path(crate::common::text(hook, "command"), "bash")
                && let Some(root) = path.parent().and_then(Path::parent)
                && dead(crate::common::text(hook, "command"), root, roots)
            {
                issues.push((
                    "drift".into(),
                    "dead OMS hook: its checkout no longer exists".into(),
                ));
            }
        }
        let mut seen = BTreeSet::new();
        issues.retain(|v| seen.insert(v.clone()));
        problems.insert(key, issues);
    }
    Ok((problems, found.into_keys().collect()))
}
