//! Import already-local artifact feedback as one reviewable queued instruction.
use crate::json::{J, array_of, dup, get, number, object, same, str_of, string};
use crate::queue::{self, Ctx};
use crate::{MAX_FEEDBACK, Result, UNSAFE, hash, safe_text, valid_id};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

const ACTIONS: [&str; 5] = ["select", "accept", "reject", "uncertain", "comment"];
const LEDGER: &str = "invalid or oversized feedback ledger";
const MAX_LEDGER: usize = 32 * 1024 * 1024;
const MAX_LEDGER_ROWS: usize = 10000;

pub fn digest(value: &str) -> Result<&str> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(value)
    } else {
        Err("expected a full SHA-256 digest")
    }
}
fn ledger(c: &Ctx) -> Result<Vec<J>> {
    let path = Path::new(".oms/artifact-views/feedback.jsonl");
    let Some(data) = crate::read_opt(c.root, path, MAX_LEDGER)? else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    let (mut start, mut i) = (0, 0);
    let mut lines = Vec::new();
    while i < data.len() {
        if data[i] == b'\n' || data[i] == b'\r' {
            lines.push(&data[start..i]);
            if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    for line in lines {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.len() > 1024 * 1024 || rows.len() >= MAX_LEDGER_ROWS {
            return Err(LEDGER);
        }
        let row = crate::json::parse(line).map_err(|_| LEDGER)?;
        if !matches!(row, J::Object(_)) {
            return Err(LEDGER);
        }
        rows.push(row);
    }
    Ok(rows)
}
fn chars(value: Option<&J>, limit: usize) -> bool {
    matches!(value, Some(J::String(b)) if std::str::from_utf8(b).is_ok_and(|s| s.chars().count() <= limit))
}

struct Batch {
    row: J,
    canonical: J,
    registry: J,
}
fn batch(c: &Ctx, requested: &str) -> Result<Batch> {
    digest(requested)?;
    let found: Vec<J> = ledger(c)?
        .into_iter()
        .filter(|row| str_of(row, "batch_id") == Some(requested))
        .collect();
    let Some(row) = found.first() else {
        return Err("feedback batch is not in the local artifact-view import ledger");
    };
    for other in &found[1..] {
        for field in [
            "schema",
            "batch_id",
            "view_id",
            "data_sha256",
            "events",
            "report",
        ] {
            let null = J::Null;
            if !same(
                get(row, field).unwrap_or(&null),
                get(other, field).unwrap_or(&null),
            ) {
                return Err("feedback batch has conflicting import records");
            }
        }
    }
    if get(row, "schema") != Some(&number(1)) {
        return Err("unsupported feedback schema");
    }
    let view = valid_id(OsStr::new(
        str_of(row, "view_id").ok_or("expected an exact local identifier")?,
    ))?;
    let data_sha = digest(str_of(row, "data_sha256").ok_or("expected a full SHA-256 digest")?)?;
    let (Some(events), Some(report @ J::Object(report_fields))) = (
        array_of(row, "events").filter(|e| e.len() <= 1000),
        get(row, "report"),
    ) else {
        return Err("invalid or oversized feedback batch");
    };
    if report_fields
        .iter()
        .any(|(k, _)| !matches!(k.as_slice(), b"title" | b"question" | b"decision" | b"next"))
    {
        return Err("unknown feedback report field");
    }
    if report_fields.iter().any(|(_, v)| !chars(Some(v), 12000)) {
        return Err("feedback report fields must be strings of at most 12000 characters");
    }
    let mismatch = "feedback does not match the registered artifact view";
    let registry_path = PathBuf::from(".oms/artifact-views").join(format!("{view}.json"));
    let registry = crate::json::parse(
        &crate::read_opt(c.root, &registry_path, 1024 * 1024)?.ok_or(mismatch)?,
    )?;
    let keys = array_of(&registry, "row_keys")
        .filter(|k| k.len() <= 100000)
        .filter(|_| {
            get(&registry, "schema") == Some(&number(1))
                && str_of(&registry, "id") == Some(view)
                && str_of(&registry, "data_sha256") == Some(data_sha)
        })
        .ok_or(mismatch)?;
    if keys.iter().any(|k| !chars(Some(k), 500)) {
        return Err("invalid registered artifact row keys");
    }
    for event in events {
        let J::Object(fields) = event else {
            return Err("feedback event has an invalid row identity, action, or comment");
        };
        let valid = fields
            .iter()
            .all(|(k, _)| matches!(k.as_slice(), b"row_key" | b"action" | b"comment"))
            && get(event, "row_key").is_some_and(|k| keys.contains(k))
            && str_of(event, "action").is_some_and(|a| ACTIONS.contains(&a))
            && get(event, "comment").is_none_or(|v| chars(Some(v), 4000));
        if !valid {
            return Err("feedback event has an invalid row identity, action, or comment");
        }
    }
    let canonical = object([
        ("view_id", string(view)),
        ("data_sha256", string(data_sha)),
        ("events", J::Array(events.iter().map(dup).collect())),
        ("report", dup(report)),
    ]);
    let encoded = crate::json::canonical(&canonical)?;
    if encoded.len() > MAX_FEEDBACK || hash(&encoded) != requested {
        return Err("feedback batch digest or byte bound does not match");
    }
    safe_text(
        std::str::from_utf8(&encoded).map_err(|_| UNSAFE)?,
        MAX_FEEDBACK,
    )?;
    // Escaped quotes can hide a credential assignment from the serialized scan.
    reject_decoded_secrets(&canonical)?;
    Ok(Batch {
        row: dup(row),
        canonical,
        registry,
    })
}

pub fn enqueue(c: &Ctx, requested: &str, room: &str, who: &str, to: &str, mode: &str) -> Result<J> {
    let Batch {
        row,
        canonical,
        registry,
    } = batch(c, requested)?;
    let route = queue::binding(c, room, who, to)?;
    let view = str_of(&row, "view_id").ok_or(UNSAFE)?;
    let events = array_of(&row, "events").ok_or(UNSAFE)?;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for event in events {
        *counts
            .entry(str_of(event, "action").ok_or(UNSAFE)?)
            .or_default() += 1;
    }
    let mut text = format!(
        "Artifact feedback for {view}.\nBatch: {requested}\nSelections and decisions: {}.",
        counts
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let next = get(&row, "report")
        .and_then(|r| str_of(r, "next"))
        .unwrap_or("");
    if !next.trim().is_empty() {
        text.push_str("\n\nRequested next step:\n");
        text.push_str(next);
    }
    text.push_str(
        "\n\nRead the attached batch for every exact selected row, comment, and report field.",
    );
    // Room for the immutable attachment reference stays reserved so a long
    // next-step report is never silently truncated on the way to a worker.
    if text.len() > 2900 {
        return Err(
            "feedback next step exceeds the room input budget; attach the full batch and write a shorter instruction",
        );
    }
    safe_text(&text, 2900)?;
    queue::under_lock(c, || {
        let mut value = queue::state(c)?;
        let previous = get(get(&value, "feedback_batches").ok_or(UNSAFE)?, requested)
            .filter(|p| !matches!(p, J::Object(f) if f.is_empty()))
            .map(dup);
        if let Some(previous) = previous {
            let existing = array_of(&value, "items")
                .ok_or(UNSAFE)?
                .iter()
                .find(|i| str_of(i, "id") == str_of(&previous, "input_id"))
                .map(dup);
            let compatible = existing.as_ref().is_some_and(|e| {
                get(e, "binding")
                    .and_then(|b| get(b, "identity"))
                    .zip(get(&route, "identity"))
                    .is_some_and(|(a, b)| same(a, b))
                    && str_of(e, "mode") == Some(mode)
            });
            let Some(existing) = existing.filter(|_| compatible) else {
                return Err(
                    "feedback batch is already mapped to a different target, identity, or mode",
                );
            };
            return Ok(object([
                ("batch_id", string(requested)),
                ("input", existing),
                ("deduplicated", J::Bool(true)),
            ]));
        }
        if array_of(&value, "items").ok_or(UNSAFE)?.len() >= 256 {
            return Err("input queue has reached its 256-item bound");
        }
        let content = std::str::from_utf8(&crate::json::canonical(&canonical)?)
            .map_err(|_| UNSAFE)?
            .to_owned();
        let record = crate::create(
            c.repo,
            "artifact-feedback",
            object([
                ("verified", J::Bool(true)),
                ("view_id", string(view)),
                ("data_sha256", dup(get(&row, "data_sha256").ok_or(UNSAFE)?)),
                ("batch_id", string(requested)),
                (
                    "registry_sha256",
                    string(&hash(&crate::json::canonical(&registry)?)),
                ),
                (
                    "note",
                    string(
                        "Explicit local feedback import; selections are data, not approval or execution authority.",
                    ),
                ),
            ]),
            &content,
            c.interrupted,
        )?;
        let selected: Vec<J> = events
            .iter()
            .filter(|e| matches!(str_of(e, "action"), Some("select" | "accept")))
            .filter_map(|e| get(e, "row_key").map(dup))
            .collect();
        let queued = queue::make_item(
            c,
            route,
            &text,
            mode,
            &[str_of(&record, "id").ok_or(UNSAFE)?.to_owned()],
            Some(object([
                ("kind", string("artifact-feedback")),
                ("batch_id", string(requested)),
                ("view_id", string(view)),
                ("data_sha256", dup(get(&row, "data_sha256").ok_or(UNSAFE)?)),
                ("selected_rows", J::Array(selected)),
            ])),
        )?;
        let queued_id = str_of(&queued, "id").ok_or(UNSAFE)?.to_owned();
        match crate::json::get_mut(&mut value, "items") {
            Some(J::Array(items)) => items.push(dup(&queued)),
            _ => return Err(UNSAFE),
        }
        set_batch(&mut value, requested, &queued_id, view, &row)?;
        queue::write_state(c, &mut value)?;
        Ok(object([
            ("batch_id", string(requested)),
            ("input", queued),
            ("deduplicated", J::Bool(false)),
        ]))
    })
}
fn set_batch(value: &mut J, requested: &str, input: &str, view: &str, row: &J) -> Result<()> {
    let J::Object(fields) = crate::json::get_mut(value, "feedback_batches").ok_or(UNSAFE)? else {
        return Err(UNSAFE);
    };
    let entry = object([
        ("input_id", string(input)),
        ("view_id", string(view)),
        ("data_sha256", dup(get(row, "data_sha256").ok_or(UNSAFE)?)),
    ]);
    fields.retain(|(k, _)| k != requested.as_bytes());
    fields.push((requested.as_bytes().to_vec(), entry));
    Ok(())
}

/// Scan every decoded object key and string value, however deeply nested.
fn reject_decoded_secrets(value: &J) -> Result<()> {
    match value {
        J::String(bytes) => crate::reject_secrets(std::str::from_utf8(bytes).map_err(|_| UNSAFE)?),
        J::Array(values) => values.iter().try_for_each(reject_decoded_secrets),
        J::Object(fields) => fields.iter().try_for_each(|(key, v)| {
            crate::reject_secrets(std::str::from_utf8(key).map_err(|_| UNSAFE)?)?;
            reject_decoded_secrets(v)
        }),
        _ => Ok(()),
    }
}
