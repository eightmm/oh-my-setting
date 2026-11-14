//! Content/mode inventory excluding only the documented live-session ambient state.
use crate::json::{number, string};
use crate::{Result, Value, error, json, require, storage};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
const AMBIENT: &[&str] = &[
    "plan/autopilot-shadow.jsonl",
    "graph/shadow.jsonl",
    "lifecycle/events.jsonl",
];
/// The inventory is observational; symlink destinations are recorded without traversal.
pub fn inventory(root: &Path) -> Result<Vec<Value>> {
    if !root.exists() {
        return Ok(vec![]);
    }
    require(
        root.symlink_metadata().map_err(|e| error(2, e))?.is_dir(),
        "inventory root must be a directory",
    )?;
    let mut stack = vec![root.to_owned()];
    let mut rows = Vec::new();
    let mut seen = 0;
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|e| error(2, e))? {
            let entry = entry.map_err(|e| error(2, e))?;
            seen += 1;
            require(seen <= 250000, "inventory entry limit exceeded")?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| error(2, "inventory escaped root"))?
                .to_string_lossy()
                .replace('\\', "/");
            let top = directory == root;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if top
                && [
                    "hooks",
                    "work-journal",
                    "project-graph",
                    "ci.jsonl",
                    ".gitignore",
                ]
                .contains(&name.as_ref())
                || AMBIENT.contains(&relative.as_str())
            {
                continue;
            }
            let (kind, mode, payload) = match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    let mode = meta.permissions().mode() & 0o7777;
                    if meta.is_symlink() {
                        (
                            "link",
                            mode,
                            string(
                                std::fs::read_link(&path)
                                    .map_err(|e| error(2, e))?
                                    .to_string_lossy(),
                            ),
                        )
                    } else if meta.is_dir() {
                        stack.push(path);
                        ("dir", mode, string(""))
                    } else if meta.is_file() {
                        match storage::hash_input(&path, u64::MAX) {
                            Ok(digest) => ("file", mode, string(digest)),
                            Err(_) => ("error", 0, string("OSError")),
                        }
                    } else {
                        ("other", mode, string(""))
                    }
                }
                Err(_) => ("error", 0, string("OSError")),
            };
            rows.push(Value::Array(vec![
                string(relative),
                string(kind),
                number(mode),
                payload,
            ]));
        }
    }
    let listed: BTreeSet<_> = rows
        .iter()
        .filter_map(|r| {
            r.array()
                .ok()
                .and_then(|r| r.first())
                .and_then(|s| s.str().ok())
        })
        .map(str::to_owned)
        .collect();
    rows.retain(|r| {
        let row = r.array().unwrap_or(&[]);
        let path = row.first().and_then(|v| v.str().ok()).unwrap_or("");
        let kind = row.get(1).and_then(|v| v.str().ok()).unwrap_or("");
        let prefix = format!("{path}/");
        kind != "dir"
            || !AMBIENT.iter().any(|p| p.starts_with(&prefix))
            || listed.iter().any(|p| p.starts_with(&prefix))
    });
    rows.sort_by(|a, b| {
        a.array()
            .unwrap_or(&[])
            .first()
            .and_then(|v| v.str().ok())
            .cmp(&b.array().unwrap_or(&[]).first().and_then(|v| v.str().ok()))
    });
    Ok(rows)
}
/// JSONL representation matches the independent inventory helper's tuple rows.
pub fn render(root: &Path) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for row in inventory(root)? {
        out.extend(json::encoded(&row)?);
    }
    Ok(out)
}
