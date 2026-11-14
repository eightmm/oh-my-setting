//! Artifact history retention preserves whole resolution lineages and receipts.
use crate::{Context, Result, Value, epoch, error, json, parse_timestamp, require, storage};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const DURABLE_BYTES: usize = 16 * 1024 * 1024;

fn integer(row: &Value, key: &str) -> Option<i64> {
    match row.get(key) {
        Value::Number(n) => n.parse().ok(),
        _ => None,
    }
}

fn outcome(row: &Value) -> bool {
    integer(row, "schema") == Some(1)
        && ["event_id", "operation_id", "artifact_id", "ts", "kind"]
            .iter()
            .all(|key| !row.text(key).is_empty())
        && matches!(row.get("provider"), Value::String(_))
        && integer(row, "exit").is_some_and(|n| n >= 0)
}

fn finite_json(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(n) if n == "NaN" || n.ends_with("Infinity") => return false,
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.iter().map(|(_, value)| value)),
            _ => {}
        }
    }
    true
}

pub(crate) fn receipts_available(ctx: &Context<'_>) -> bool {
    receipt_pins(ctx).is_ok()
}

// A corrupt or unbounded receipt namespace protects all history. It is never
// interpreted as an empty set of pins.
fn receipt_pins(ctx: &Context<'_>) -> Result<BTreeSet<String>> {
    let directory = ctx.repo.join(".oms/plan/completions");
    let meta = match std::fs::symlink_metadata(&directory) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(error(2, e)),
    };
    require(
        meta.is_dir() && !meta.is_symlink(),
        "unsafe completion receipt namespace",
    )?;
    let mut pins = BTreeSet::new();
    let mut total = 0usize;
    for (count, entry) in std::fs::read_dir(&directory)
        .map_err(|e| error(2, e))?
        .enumerate()
    {
        require(count < 4096, "completion receipt count exceeded")?;
        let entry = entry.map_err(|e| error(2, e))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| error(2, "invalid receipt name"))?;
        let digest = name.strip_suffix(".json").unwrap_or_default();
        require(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid receipt name",
        )?;
        let raw = storage::read(&ctx.repo, &entry.path(), 4 * 1024 * 1024, false)?;
        total = total.saturating_add(raw.len());
        require(
            total <= 64 * 1024 * 1024,
            "completion receipt byte bound exceeded",
        )?;
        require(
            format!("{:x}", Sha256::digest(&raw)) == digest,
            "completion receipt digest changed",
        )?;
        let receipt = json::parse(&raw)?;
        let bundle = receipt.get("bundle");
        require(bundle.object().is_ok(), "invalid completion receipt bundle")?;
        for key in ["research", "main_research"] {
            if let Some(info) = bundle.opt(key) {
                require(
                    info.object().is_ok()
                        && info.text("classification") == "research-only"
                        && !info.text("index_event_id").is_empty()
                        && !info.get("artifact").text("path").is_empty(),
                    "invalid completion evidence pin",
                )?;
                require(
                    receipt
                        .get("snapshot")
                        .get("evidence")
                        .get("index")
                        .object()
                        .is_ok(),
                    "invalid completion index snapshot",
                )?;
                pins.insert(info.text("index_event_id").to_owned());
            }
        }
    }
    Ok(pins)
}

fn lineage(rows: &[Value], by_id: &BTreeMap<String, usize>, index: usize) -> Option<Vec<usize>> {
    let mut group = Vec::new();
    let mut current = index;
    loop {
        group.push(current);
        let row = &rows[current];
        if row.text("kind") != "artifact-resolution" {
            return Some(group);
        }
        let target = *by_id.get(row.text("resolves_event_id"))?;
        if target >= current {
            return None;
        }
        current = target;
    }
}

pub(crate) fn prune(ctx: &Context<'_>, apply: bool) -> Result<(usize, Option<String>)> {
    let path = Path::new(".oms/artifacts/index.jsonl");
    if !ctx.repo.join(path).exists() {
        return Ok((0, None));
    }
    let keep = match std::env::var("OMS_ARTIFACT_INDEX_KEEP") {
        Ok(raw) => raw.parse::<usize>().unwrap_or(1000),
        Err(_) => 1000,
    };
    require(keep > 0, "artifact retention requires positive keep")?;
    let _guard = storage::lock(&ctx.repo.join(path))?;
    let raw = storage::read(&ctx.repo, path, storage::MAX_STREAM, false)?;
    require(
        raw.is_empty() || raw.ends_with(b"\n"),
        "partial artifact index; pruning refused",
    )?;
    let lines = raw.split_inclusive(|b| *b == b'\n').collect::<Vec<_>>();
    let mut rows = Vec::with_capacity(lines.len());
    let mut ids: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        require(
            line.len() <= 1024 * 1024,
            "artifact index row exceeds 1 MiB",
        )?;
        let payload = line.strip_suffix(b"\n").unwrap_or(line);
        let payload = payload.strip_suffix(b"\r").unwrap_or(payload);
        require(
            !payload.contains(&b'\r') && !payload.contains(&0) && !payload.trim_ascii().is_empty(),
            "invalid artifact index physical row",
        )?;
        let row = json::parse_legacy(payload, 1024 * 1024)?;
        require(row.object().is_ok(), "artifact index row is not an object")?;
        require(
            finite_json(&row),
            "artifact index contains non-finite constants",
        )?;
        if matches!(row.get("event_id"), Value::String(_)) {
            ids.entry(row.get("event_id").str()?.to_owned())
                .or_default()
                .push(index);
        }
        rows.push(row);
    }
    if rows.len() <= keep && raw.len() <= DURABLE_BYTES {
        return Ok((0, None));
    }
    let pins = match receipt_pins(ctx) {
        Ok(pins) => pins,
        Err(_) => {
            return Ok((
                0,
                Some("artifact rows retained: completion receipts unavailable".into()),
            ));
        }
    };
    let by_id = ids
        .iter()
        .filter(|(_, indices)| indices.len() == 1)
        .map(|(id, indices)| (id.clone(), indices[0]))
        .collect::<BTreeMap<_, _>>();
    let mut targets = BTreeSet::new();
    let mut settled = BTreeSet::new();
    for (index, row) in rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.text("kind") == "artifact-resolution")
    {
        if let Some(&target_index) = by_id.get(row.text("resolves_event_id"))
            && target_index < index
        {
            targets.insert(target_index);
            let target = &rows[target_index];
            if outcome(row)
                && outcome(target)
                && by_id.get(row.text("event_id")) == Some(&index)
                && row.text("parent_event_id") == row.text("resolves_event_id")
                && row.text("resolution") == "resolved"
                && integer(row, "exit") == Some(0)
                && target.text("kind") != "artifact-resolution"
                && integer(target, "exit").is_some_and(|n| n > 0)
                && row.get("operation_id") == target.get("operation_id")
                && row.get("artifact_id") == target.get("artifact_id")
            {
                settled.insert(target_index);
            }
        }
    }
    let mut selected = BTreeSet::new();
    for id in pins {
        for &index in ids.get(&id).into_iter().flatten() {
            match lineage(&rows, &by_id, index) {
                Some(group) => selected.extend(group),
                None => {
                    return Ok((
                        0,
                        Some("artifact rows retained: ambiguous pinned lineage".into()),
                    ));
                }
            }
        }
    }
    for (index, row) in rows.iter().enumerate() {
        if row.text("kind") != "artifact-resolution"
            && integer(row, "exit").is_some_and(|n| n > 0)
            && !settled.contains(&index)
        {
            selected.insert(index);
        }
    }
    let mut bytes = selected.iter().map(|&i| lines[i].len()).sum::<usize>();
    require(
        bytes <= DURABLE_BYTES,
        "protected artifact rows exceed durable byte ceiling",
    )?;
    let now = epoch();
    let decisions = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.text("kind") == "panel-result"
                && parse_timestamp(row.text("ts"))
                    .is_some_and(|t| t >= now - 30 * 86400 && t <= now + 300)
        })
        .map(|(i, _)| i)
        .rev()
        .take(3000);
    for index in decisions {
        if !selected.contains(&index) && bytes + lines[index].len() <= DURABLE_BYTES {
            selected.insert(index);
            bytes += lines[index].len();
        }
    }
    let mut ordinary = 0usize;
    for index in (0..rows.len()).rev() {
        if selected.contains(&index) || targets.contains(&index) {
            continue;
        }
        let Some(group) = lineage(&rows, &by_id, index) else {
            continue;
        };
        let group = group
            .into_iter()
            .filter(|i| !selected.contains(i))
            .collect::<Vec<_>>();
        let size = group.iter().map(|&i| lines[i].len()).sum::<usize>();
        if ordinary.saturating_add(group.len()) > keep || bytes.saturating_add(size) > DURABLE_BYTES
        {
            continue;
        }
        ordinary += group.len();
        bytes += size;
        selected.extend(group);
        if ordinary == keep || bytes == DURABLE_BYTES {
            break;
        }
    }
    let removed = rows.len() - selected.len();
    if apply && removed > 0 {
        let body = selected
            .into_iter()
            .flat_map(|i| lines[i].iter().copied())
            .collect::<Vec<_>>();
        storage::replace_locked(&ctx.repo, path, &body)?;
    }
    Ok((removed, None))
}
