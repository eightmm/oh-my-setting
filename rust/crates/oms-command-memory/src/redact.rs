use crate::{
    records,
    store::{self, Store},
    *,
};
use std::{path::Path, sync::atomic::AtomicBool};
fn boundary(raw: &[u8], e: &records::Entry) -> Result<()> {
    if e.source != "shared" {
        return Ok(());
    }
    let tail = String::from_utf8_lossy(&raw[e.end..]);
    if tail.lines().next().is_some_and(|l| l.starts_with("## ")) {
        return err("memory database: ambiguous memory entry boundary; nothing was changed");
    }
    let prefix = String::from_utf8_lossy(&raw[..e.start]);
    if prefix.lines().any(|l| l.starts_with("## "))
        && prefix
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .is_none_or(|l| l.trim() != "-->")
    {
        return err("memory database: ambiguous memory entry boundary; nothing was changed");
    }
    Ok(())
}
fn replacement(raw: &[u8], e: &records::Entry, agent: &str) -> Result<Vec<u8>> {
    boundary(raw, e)?;
    let removed = removed_size(raw, e);
    let author: String = agent
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
        .collect();
    let tombstone = format!(
        "[redacted {} by {}: text removed with `oms agent-memory redact`; {removed} bytes]",
        now(),
        if author.is_empty() {
            "unknown"
        } else {
            &author
        }
    );
    let mut before = raw[..e.start].to_vec();
    if e.event_id.starts_with("legacy:") {
        let prefix = String::from_utf8_lossy(&before);
        let last = prefix.lines().rev().find(|l| !l.trim().is_empty());
        if last.is_some_and(|l| l.trim() == "-->") {
            if let Some(pos) = before.windows(3).rposition(|w| w == b"-->") {
                before.splice(pos..pos, format!("event_id: {}\n", e.event_id).bytes());
            }
        } else {
            before
                .extend(format!("{}\nevent_id: {}\n-->\n", records::START, e.event_id).as_bytes());
        }
    }
    if e.source == "pins" {
        before.extend(format!("- {} [{}] {tombstone}\n", e.occurred_at, e.agent).as_bytes());
    } else {
        let end = raw[e.start..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| e.start + i + 1)
            .unwrap_or(e.end);
        before.extend(&raw[e.start..end]);
        before.extend(format!("\n{tombstone}\n\n").as_bytes());
        if !e.field("supersedes").is_empty() || !e.field("retires").is_empty() {
            before.extend(format!("{}\n", records::COMPLETE).as_bytes());
        }
        if e.end < raw.len() {
            before.push(b'\n');
        }
    }
    before.extend(&raw[e.end..]);
    Ok(before)
}
fn locate(s: &Store, id: &str) -> Result<(std::path::PathBuf, Vec<u8>, records::Entry)> {
    let mut found = Vec::new();
    for (source, path) in [("shared", &s.shared), ("pins", &s.pins)] {
        let raw = store::read(path)?;
        for e in records::parse(&raw, source) {
            if e.event_id == id {
                found.push((path.clone(), raw.clone(), e));
            }
        }
    }
    if !records::valid_id(id) || found.len() != 1 {
        return err("memory database: unknown or ambiguous memory ID");
    }
    let found = found.remove(0);
    boundary(&found.1, &found.2)?;
    Ok(found)
}
pub(crate) fn redact(
    s: &Store,
    id: &str,
    agent: &str,
    apply: bool,
    flag: &AtomicBool,
) -> Result<Output> {
    if !records::valid_id(id) {
        return err("invalid memory ID");
    }
    let _shared = if apply {
        Some(store::lock(&s.shared, flag)?)
    } else {
        None
    };
    let _pins = if apply {
        Some(store::lock(&s.pins, flag)?)
    } else {
        None
    };
    let (path, raw, e) = locate(s, id)?;
    let label = format!(
        "{} {} ({id}, {} {})",
        e.source, e.kind, e.occurred_at, e.agent
    );
    let already = records::redacted(&e.body);
    if !apply {
        return Ok(crate::workflows::output(if already {
            format!("memory: already redacted: {label}")
        } else {
            format!(
                "memory: would redact {label}: {} bytes\nmemory: dry run; nothing changed. Re-run with --apply to remove the text.",
                removed_size(&raw, &e)
            )
        }));
    }
    let _db = store::lock(&s.database, flag)?;
    let _summary = store::lock(&s.summary, flag)?;
    // Invalidate derived copies before removing source text. A killed operation
    // leaves either the old authority or a tombstone, with no surviving index.
    store::remove(&s.summary)?;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut path = s.database.as_os_str().to_owned();
        path.push(suffix);
        store::remove(Path::new(&path))?;
    }
    let mut out = if already {
        format!("memory: already redacted: {label}\n").into_bytes()
    } else {
        let data = replacement(&raw, &e, agent)?;
        store::mutate(&path, flag, |current| {
            if current != raw {
                return err("memory source changed during redaction; nothing was changed");
            }
            Ok(data)
        })?;
        let mut bytes = format!(
            "memory: redacted {label}: {} bytes removed from ",
            removed_size(&raw, &e)
        )
        .into_bytes();
        bytes.extend(path.file_name().unwrap_or_default().as_encoded_bytes());
        bytes.push(b'\n');
        bytes
    };
    drop(_summary);
    let warning = if s.compact(flag).is_err() {
        b"warning: summary.md was removed and could not be regenerated; run: oms agent-memory compact\n".to_vec()
    } else {
        Vec::new()
    };
    if db::rebuild_locked(s, flag).is_err() {
        return Ok((1,out,b"error: the text was removed from the source log but the index was not rebuilt; run: oms agent-memory rebuild\n".to_vec()));
    }
    out.extend_from_slice(b"memory: summary.md and the index were rebuilt without it\nmemory: copies outside the memory store (provider transcripts, handoffs, backups, disk blocks) are not touched\n");
    Ok((0, out, warning))
}

fn removed_size(raw: &[u8], e: &records::Entry) -> usize {
    let ranges = records::line_ranges(&raw[e.start..e.end]);
    if e.source == "pins" {
        let line = &raw[e.start..e.start + ranges.first().map(|r| r.1).unwrap_or(0)];
        return line
            .windows(2)
            .position(|w| w == b"] ")
            .map(|at| line.len() - at - 2)
            .unwrap_or(e.body.len());
    }
    let mut rows: Vec<&[u8]> = ranges
        .iter()
        .skip(1)
        .map(|(a, b)| &raw[e.start + a..e.start + b])
        .collect();
    if e.field("link_complete") == "required" {
        while rows.last().is_some_and(|r| r.trim_ascii().is_empty()) {
            rows.pop();
        }
        if rows.last() == Some(&records::COMPLETE.as_bytes()) {
            rows.pop();
        }
    }
    let body = rows.join(&b'\n');
    let body = body.trim_ascii();
    std::str::from_utf8(body)
        .map(|s| s.trim().len())
        .unwrap_or(body.len())
}
