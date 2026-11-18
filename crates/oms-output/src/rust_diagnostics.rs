//! Cargo JSON diagnostics are rendered as bounded, explicitly lossy blocks.
//! Unknown records and mixed stderr remain ordinary output, never instructions.
use super::*;
use serde_json::Value;

const RECORD_CAP: usize = 1024 * 1024;
const RECORDS_CAP: usize = 10_000;

struct Block {
    text: String,
    source_bytes: usize,
    diagnostic: bool,
    failure: bool,
    transformations: Transformations,
}

fn diagnostic(line: &[u8], max_bytes: usize) -> Option<(String, bool)> {
    if line.len() > RECORD_CAP {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    if value.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }
    let message = value.get("message")?;
    let level = message.get("level")?.as_str()?;
    if !matches!(
        level,
        "error" | "warning" | "note" | "help" | "failure-note"
    ) {
        return None;
    }
    let title = message.get("message")?.as_str()?;
    let spans = message.get("spans")?.as_array()?;
    let children = message.get("children")?.as_array()?;
    let headline = |level: &str, title: &str, code: Option<&str>| match code {
        Some(code) => clean(&format!("{level}[{code}]: {title}")),
        None => clean(&format!("{level}: {title}")),
    };
    let code = message
        .get("code")
        .and_then(|v| v.get("code"))
        .and_then(Value::as_str);
    let summary = headline(level, title, code);
    let mut text = String::new();
    let mut add = |line: &str| {
        if text.len() <= max_bytes {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&clean(line));
        }
    };
    if let Some(rendered) = message
        .get("rendered")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        for line in rendered.lines() {
            add(line);
        }
    } else {
        add(&summary);
        let span_line = |span: &Value| -> Option<String> {
            Some(format!(
                " --> {}:{}:{}{}",
                span.get("file_name")?.as_str()?,
                span.get("line_start")?.as_u64()?,
                span.get("column_start")?.as_u64()?,
                span.get("label")
                    .and_then(Value::as_str)
                    .map_or(String::new(), |label| format!(": {label}"))
            ))
        };
        for span in spans {
            if let Some(line) = span_line(span) {
                add(&line);
            }
        }
        for child in children {
            if let (Some(level), Some(title)) = (
                child.get("level").and_then(Value::as_str),
                child.get("message").and_then(Value::as_str),
            ) {
                add(&headline(level, title, None));
                if let Some(spans) = child.get("spans").and_then(Value::as_array) {
                    for span in spans {
                        if let Some(line) = span_line(span) {
                            add(&line);
                        }
                    }
                }
            }
        }
    }
    // Never present the prefix of a large diagnostic as a complete block.
    if text.len() > max_bytes {
        text = format!("{summary}\n[diagnostic exceeds the view budget; read the raw record]");
    }
    Some((text, matches!(level, "error" | "failure-note")))
}

pub(super) fn render(
    bytes: &[u8],
    options: &Options,
    original: &OutputDigest,
) -> Option<OutputDigest> {
    let total = original.raw.lines;
    if total > RECORDS_CAP {
        return None;
    }
    let mut blocks = Vec::new();
    let mut by = serde_json::Map::new();
    let mut diagnostics = 0;
    for (index, line) in bytes.split(|&b| b == b'\n').take(total).enumerate() {
        let source_bytes =
            line.len() + usize::from(index + 1 < total || !original.raw.partial_last_line);
        if let Some((text, failure)) = diagnostic(line, options.max_bytes) {
            diagnostics += 1;
            if failure {
                let count = by.entry("rust-diagnostic").or_insert(serde_json::json!(0));
                *count = serde_json::json!(count.as_u64().unwrap() + 1);
            }
            blocks.push(Block {
                text,
                source_bytes,
                diagnostic: true,
                failure,
                transformations: Transformations::default(),
            });
        } else {
            let decoded = String::from_utf8_lossy(line);
            let projected = recogniser_text(&ansi().replace_all(&decoded, ""));
            let recogniser = recognisers()
                .iter()
                .find(|(_, pattern)| pattern.is_match(&projected));
            if let Some((name, _)) = recogniser {
                let count = by.entry(*name).or_insert(serde_json::json!(0));
                *count = serde_json::json!(count.as_u64().unwrap() + 1);
            }
            let (text, transformations) = details(line);
            blocks.push(Block {
                text,
                source_bytes,
                diagnostic: false,
                failure: recogniser.is_some(),
                transformations,
            });
        }
    }
    if diagnostics == 0 {
        return None;
    }
    let mut doc = original.clone();
    // The representation is not source-lossless, even when every diagnostic fits.
    doc.representation.algorithm = "rust-diagnostics";
    doc.representation.rows = Some(diagnostics);
    doc.transformations.json_formatting = true;
    doc.parser = by.keys().cloned().collect();
    if !doc.parser.iter().any(|name| name == "rust-diagnostic") {
        doc.parser.push("rust-diagnostic".into());
        doc.parser.sort();
    }
    doc.failures.by_recogniser = by;
    doc.failures.matched = doc
        .failures
        .by_recogniser
        .values()
        .map(|v| v.as_u64().unwrap() as usize)
        .sum();
    let mut selected = vec![false; blocks.len()];
    // Reserve space for an explicit omission marker, regardless of its final count.
    let marker = "[one or more source records omitted; read the raw capture]";
    let all_bytes = blocks
        .iter()
        .map(|b| b.text.len() + 1)
        .sum::<usize>()
        .saturating_sub(1);
    if all_bytes <= options.max_bytes {
        selected.fill(true);
    } else {
        let mut remaining = options.max_bytes.saturating_sub(marker.len() + 1);
        let mut take = |index: usize| {
            if !selected[index] && blocks[index].text.len() < remaining {
                remaining -= blocks[index].text.len() + 1;
                selected[index] = true;
            }
        };
        for (index, _) in blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.failure)
            .take(FAILURE_LINES)
        {
            take(index);
        }
        for (index, _) in blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.diagnostic && !b.failure)
            .take(FAILURE_LINES)
        {
            take(index);
        }
        for index in (total.saturating_sub(options.tail)..total).rev() {
            take(index);
        }
        for index in 0..options.head.min(total) {
            take(index);
        }
    }
    let mut out = Vec::new();
    let mut retained = 0;
    for (block, &selected) in blocks.iter().zip(&selected) {
        if !selected {
            continue;
        }
        out.push(block.text.as_str());
        if block.failure {
            doc.failures.shown += 1;
        }
        if !block.diagnostic {
            let t = &block.transformations;
            doc.transformations.normalized_bytes += t.normalized_bytes;
            doc.transformations.clipped_bytes += t.clipped_bytes;
            doc.transformations.clipped_chars += t.clipped_chars;
            doc.transformations.clipped_lines += t.clipped_lines;
            doc.transformations.invalid_utf8_bytes += t.invalid_utf8_bytes;
            retained += block
                .source_bytes
                .saturating_sub(t.normalized_bytes + t.clipped_bytes + t.invalid_utf8_bytes);
        }
    }
    let shown = selected.iter().filter(|&&s| s).count();
    if shown < total {
        out.push(marker);
    }
    doc.text = out.join("\n");
    doc.shown = Shown {
        mode: "rust-diagnostics".into(),
        lines: shown,
        omitted_lines: total - shown,
        omitted_bytes: original.raw.bytes - retained,
    };
    Some(doc)
}
