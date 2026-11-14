//! Bounded patch inventories and descriptor-backed live file reads.
use crate::{Error, Result, Store, Value, require};
use std::collections::BTreeSet;
use std::path::Path;
pub const FILE_LIMIT: usize = 1024 * 1024;
#[derive(Clone, Debug, serde::Serialize)]
pub struct Change {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}
fn patch_name(raw: &str, prefix: bool) -> Result<Option<String>> {
    if raw == "/dev/null" {
        return Ok(None);
    }
    let decoded = if raw.starts_with('"') {
        require(
            raw.ends_with('"') && raw.len() >= 2,
            "invalid quoted Git path",
        )?;
        let raw = &raw.as_bytes()[1..raw.len() - 1];
        let mut bytes = vec![];
        let mut index = 0;
        while index < raw.len() {
            if raw[index] != b'\\' {
                bytes.push(raw[index]);
                index += 1;
                continue;
            }
            index += 1;
            require(index < raw.len(), "truncated Git path escape")?;
            if (b'0'..=b'7').contains(&raw[index]) {
                let start = index;
                while index < raw.len() && index - start < 3 && (b'0'..=b'7').contains(&raw[index])
                {
                    index += 1;
                }
                let value = u16::from_str_radix(
                    std::str::from_utf8(&raw[start..index]).map_err(Error::new)?,
                    8,
                )
                .map_err(Error::new)?;
                bytes.push(u8::try_from(value).map_err(Error::new)?);
            } else {
                bytes.push(match raw[index] {
                    b'a' => 7,
                    b'b' => 8,
                    b't' => 9,
                    b'n' => 10,
                    b'v' => 11,
                    b'f' => 12,
                    b'r' => 13,
                    b'"' => 34,
                    b'\\' => 92,
                    _ => return Err(Error::new("unsupported Git path escape")),
                });
                index += 1;
            }
        }
        String::from_utf8(bytes)
            .map_err(|_| Error::new("non-UTF-8 patch paths require the lossless owner adapter"))?
    } else {
        raw.to_owned()
    };
    let path = if prefix && (decoded.starts_with("a/") || decoded.starts_with("b/")) {
        &decoded[2..]
    } else {
        &decoded
    };
    require(
        !path.is_empty() && !path.contains('\0'),
        "invalid patch path",
    )?;
    crate::storage::path(Path::new(path))?;
    Ok(Some(path.into()))
}
pub fn patch_paths(data: &[u8]) -> Result<Vec<String>> {
    require(
        data.len() <= crate::results::PATCH_MAX,
        "patch exceeds byte limit",
    )?;
    let source = std::str::from_utf8(data)
        .map_err(|_| Error::new("non-UTF-8 patch requires the lossless owner adapter"))?;
    let header = regex::Regex::new(r#"^("(?:[^"\\]|\\.)*"|a/.*?) ("(?:[^"\\]|\\.)*"|b/.*)$"#)
        .map_err(Error::new)?;
    let hunk = regex::Regex::new(r"^@@ -[0-9]+(?:,([0-9]+))? \+[0-9]+(?:,([0-9]+))? @@")
        .map_err(Error::new)?;
    let mut paths = Vec::new();
    let mut names = Vec::new();
    let mut fallback = Vec::new();
    let mut remaining = 0usize;
    for line in source.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            paths.extend(
                if names.is_empty() { &fallback } else { &names }
                    .iter()
                    .cloned(),
            );
            names.clear();
            fallback.clear();
            remaining = 0;
            let c = header
                .captures(rest)
                .ok_or_else(|| Error::new("patch paths unavailable: invalid git header"))?;
            for raw in [&c[1], &c[2]] {
                if let Some(name) = patch_name(raw, true)? {
                    fallback.push(name);
                }
            }
        } else if line.starts_with("@@ ") {
            let c = hunk
                .captures(line)
                .ok_or_else(|| Error::new("patch paths unavailable: invalid hunk header"))?;
            let count = |i| {
                c.get(i)
                    .map(|v| v.as_str().parse::<usize>().map_err(Error::new))
                    .unwrap_or(Ok(1))
            };
            remaining = count(1)?
                .checked_add(count(2)?)
                .ok_or_else(|| Error::new("hunk count overflow"))?;
        } else if remaining > 0 {
            remaining = remaining.saturating_sub(if line.starts_with(' ') {
                2
            } else {
                usize::from(line.starts_with(['+', '-']))
            });
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            if let Some(name) = patch_name(line[4..].split('\t').next().unwrap_or_default(), true)?
            {
                names.push(name);
            }
        } else {
            for prefix in ["rename from ", "rename to ", "copy from ", "copy to "] {
                if let Some(raw) = line.strip_prefix(prefix)
                    && let Some(name) = patch_name(raw, false)?
                {
                    names.push(name);
                }
            }
        }
    }
    paths.extend(if names.is_empty() { fallback } else { names });
    let mut seen = BTreeSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    Ok(paths)
}
pub fn patch_changes(data: &[u8]) -> Result<Vec<Change>> {
    let paths = patch_paths(data)?;
    let mut changes = Vec::new();
    for path in paths {
        let section = file_diff(data, &path)?;
        let section = std::str::from_utf8(&section).map_err(Error::new)?;
        changes.push(Change {
            path,
            added: section
                .lines()
                .filter(|v| v.starts_with('+') && !v.starts_with("+++ "))
                .count(),
            removed: section
                .lines()
                .filter(|v| v.starts_with('-') && !v.starts_with("--- "))
                .count(),
        });
    }
    Ok(changes)
}
pub fn file_diff(data: &[u8], path: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(data).map_err(Error::new)?;
    let mut result = Vec::new();
    let starts: Vec<_> = text
        .match_indices("diff --git ")
        .filter(|(i, _)| *i == 0 || data[*i - 1] == b'\n')
        .map(|v| v.0)
        .chain(std::iter::once(data.len()))
        .collect();
    for range in starts.windows(2) {
        let part = &data[range[0]..range[1]];
        if patch_paths(part)?.iter().any(|v| v == path) {
            result.extend_from_slice(part);
        }
    }
    Ok(result)
}
pub fn overlaps(left: &[String], right: &[String]) -> bool {
    oms_contract_markdown::scope::scopes_overlap(
        Some(&left.iter().map(String::as_str).collect::<Vec<_>>()),
        Some(&right.iter().map(String::as_str).collect::<Vec<_>>()),
    )
}
pub fn read_live(repo: &Path, relative: &Path) -> Result<Vec<u8>> {
    Store::open(repo)?.read(relative, FILE_LIMIT, false)
}
pub fn tree_rows(changes: &[Change], declared: &[String]) -> Value {
    serde_json::json!({"files":changes,"declared":declared,"display_only":true})
}

/// Export bounded, scrubbed inventory records; raw logs and patch bodies are
/// excluded. Exclusive publication uses the existing private export writer.
pub fn export_jsonl(target: &Path, rows: &[Value]) -> Result<usize> {
    require(rows.len() <= 10000, "file export exceeds record limit")?;
    let mut body = Vec::new();
    for row in rows {
        let line = crate::value::encoded(row)?;
        require(
            line.len() <= FILE_LIMIT
                && body.len().saturating_add(line.len()) <= crate::results::PATCH_MAX,
            "file export exceeds byte limit",
        )?;
        crate::outbound(&line)?;
        body.extend(line);
    }
    crate::export::publish(target, &body)?;
    Ok(rows.len())
}
pub fn export_bundle(target: &Path, changes: &[Change], declared: &[String]) -> Result<usize> {
    let bundle = serde_json::json!({"schema":1,"kind":"oms-panel-files","files":changes,"declared":declared,"display_only":true});
    crate::export::write(
        target,
        &String::from_utf8(crate::value::encoded(&bundle)?).map_err(Error::new)?,
    )
}
