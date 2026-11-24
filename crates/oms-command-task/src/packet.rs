//! Byte-preserving durable Markdown operations. Projection helpers deliberately
//! remain separate from the exact `## ` shell writer grammar.
use crate::{Error, Result, hash};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::BTreeMap;

pub const SECTIONS: &[&str] = &[
    "Goal",
    "Constraints",
    "Done Criteria",
    "Verify",
    "Loop State",
    "Last Failure",
    "Verification",
    "Decisions",
    "Current State",
    "Next Step",
];
pub fn lines(raw: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<_> = raw.split(|b| *b == b'\n').collect();
    if raw.ends_with(b"\n") {
        v.pop();
    }
    if raw.is_empty() {
        v.clear();
    }
    v
}
pub fn join(lines: impl IntoIterator<Item = impl AsRef<[u8]>>) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line.as_ref());
        out.push(b'\n');
    }
    out
}
pub fn trim(mut s: &[u8]) -> &[u8] {
    while s.first().is_some_and(u8::is_ascii_whitespace) {
        s = &s[1..];
    }
    while s.last().is_some_and(u8::is_ascii_whitespace) {
        s = &s[..s.len() - 1];
    }
    s
}
fn value<'a>(line: &'a [u8], key: &str) -> Option<&'a [u8]> {
    line.strip_prefix(format!("- {key}:").as_bytes())
        .map(|mut v| {
            while v.first().is_some_and(u8::is_ascii_whitespace) {
                v = &v[1..];
            }
            v
        })
}
pub fn metadata<'a>(raw: &'a [u8], key: &str) -> Option<&'a [u8]> {
    lines(raw)
        .into_iter()
        .take_while(|l| !l.starts_with(b"## "))
        .find_map(|l| value(l, key))
}
pub fn section(raw: &[u8], name: &str) -> Vec<u8> {
    let heading = format!("## {name}");
    let mut inside = false;
    let mut out = Vec::new();
    for l in lines(raw) {
        if l == heading.as_bytes() {
            inside = true;
            continue;
        }
        if inside && l.starts_with(b"## ") {
            inside = false;
        }
        if inside {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
    }
    out
}
pub fn nonempty(raw: &[u8]) -> Vec<u8> {
    join(lines(raw).into_iter().filter(|l| !trim(l).is_empty()))
}
pub fn command(raw: &[u8]) -> Vec<u8> {
    let mut out = nonempty(&section(raw, "Verify"));
    while out.ends_with(b"\n") {
        out.pop();
    }
    out
}
pub fn section_value(raw: &[u8], section_name: &str, key: &str) -> Vec<u8> {
    lines(&section(raw, section_name))
        .into_iter()
        .find_map(|l| value(l, key))
        .unwrap_or_default()
        .to_vec()
}

pub fn set_metadata(raw: &[u8], key: &str, v: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut before = true;
    let mut found = false;
    let mut inserted = false;
    let mut row = format!("- {key}: ").into_bytes();
    row.extend_from_slice(v);
    for l in lines(raw) {
        if before {
            if value(l, key).is_some() {
                out.extend_from_slice(&row);
                out.push(b'\n');
                found = true;
                continue;
            }
            if (l.starts_with(b"## ") || l.starts_with(b"Short-lived handoff packet"))
                && !found
                && !inserted
            {
                out.extend_from_slice(&row);
                out.extend_from_slice(b"\n\n");
                inserted = true;
            }
            if l.starts_with(b"## ") {
                before = false;
            }
        }
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    if !found && !inserted {
        out.extend_from_slice(&row);
        out.push(b'\n');
    }
    out
}

pub fn initial(now: &str, id: &str, source: &[u8]) -> Vec<u8> {
    let mut out=format!("# Active Agent Task\n\n- created: {now}\n- updated: {now}\n- task_id: {id}\n- status: active\n- source_session: ").into_bytes();
    out.extend_from_slice(source);
    out.extend_from_slice(format!("\n- last_activity: {now}\n- closed_at:\n- owner: oh-my-setting agent harness\n\nShort-lived handoff packet for Codex, Claude Code, and Antigravity.\n").as_bytes());
    out.extend_from_slice(b"Do not store secrets, credentials, private machine paths, cluster details, raw logs, datasets, or checkpoints here.\n\n");
    for (i, name) in SECTIONS.iter().enumerate() {
        out.extend_from_slice(format!("## {name}\n").as_bytes());
        if i + 1 < SECTIONS.len() {
            out.push(b'\n');
        }
    }
    out
}
pub fn ensure(mut raw: Vec<u8>, now: &str, id: &str, source: &[u8]) -> Vec<u8> {
    for (k, v) in [
        ("created", now.as_bytes()),
        ("updated", now.as_bytes()),
        ("task_id", id.as_bytes()),
        ("status", b"active".as_slice()),
    ] {
        if metadata(&raw, k).is_none_or(|v| v.is_empty()) {
            raw = set_metadata(&raw, k, v);
        }
    }
    if !lines(&raw)
        .iter()
        .any(|l| l.starts_with(b"- source_session:"))
    {
        raw = set_metadata(&raw, "source_session", source);
    }
    if metadata(&raw, "last_activity").is_none_or(|v| v.is_empty()) {
        let updated = metadata(&raw, "updated").unwrap_or(now.as_bytes()).to_vec();
        raw = set_metadata(&raw, "last_activity", &updated);
    }
    if !lines(&raw).iter().any(|l| l.starts_with(b"- closed_at:")) {
        raw = set_metadata(&raw, "closed_at", b"");
    }
    raw
}
pub fn touch(mut raw: Vec<u8>, now: &str) -> Vec<u8> {
    raw = set_metadata(&raw, "updated", now.as_bytes());
    set_metadata(&raw, "last_activity", now.as_bytes())
}
pub fn status(mut raw: Vec<u8>, state: &str, now: &str) -> Vec<u8> {
    raw = set_metadata(&raw, "status", state.as_bytes());
    if state == "closed" {
        raw = set_metadata(&raw, "closed_at", now.as_bytes());
    }
    touch(raw, now)
}
pub fn replace(raw: &[u8], name: &str, content: &[u8]) -> Vec<u8> {
    let heading = format!("## {name}");
    let mut found = false;
    let mut skip = false;
    let mut out = Vec::new();
    let content = join(lines(content));
    for l in lines(raw) {
        if l == heading.as_bytes() {
            out.extend_from_slice(l);
            out.extend_from_slice(b"\n\n");
            out.extend_from_slice(&content);
            found = true;
            skip = true;
            continue;
        }
        if skip && l.starts_with(b"## ") {
            skip = false;
        }
        if !skip {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
    }
    if !found {
        out.extend_from_slice(format!("\n{heading}\n\n").as_bytes());
        out.extend_from_slice(&content);
    }
    out
}
pub fn flatten(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut space = false;
    for b in raw {
        if b.is_ascii_whitespace() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(b' ');
            }
            out.push(*b);
            space = false;
        }
    }
    out
}
pub fn cap_utf8(raw: &[u8], max: usize) -> Vec<u8> {
    // Equivalent to head -c followed by iconv -c: discard incomplete and invalid
    // sequences while preserving valid bytes and literal backslash escapes.
    let raw = &raw[..raw.len().min(max)];
    let mut rest = raw;
    let mut out = Vec::new();
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(_) => {
                out.extend_from_slice(rest);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                out.extend_from_slice(&rest[..good]);
                rest = &rest[good + e.error_len().unwrap_or(rest.len() - good)..];
            }
        }
    }
    out
}
pub fn append(
    raw: &[u8],
    name: &str,
    agent: &[u8],
    content: &[u8],
    now: &str,
    note_cap: usize,
    current_cap: usize,
) -> Vec<u8> {
    let content = cap_utf8(&flatten(content), note_cap);
    if content.is_empty() {
        return raw.to_vec();
    }
    let mut bullet = format!("- {now} [").into_bytes();
    bullet.extend_from_slice(agent);
    bullet.extend_from_slice(b"] ");
    bullet.extend_from_slice(&content);
    let heading = format!("## {name}");
    let mut found = false;
    let mut inside = false;
    let mut inserted = false;
    let mut out = Vec::new();
    for l in lines(raw) {
        if l == heading.as_bytes() {
            found = true;
            inside = true;
            out.extend_from_slice(l);
            out.push(b'\n');
            continue;
        }
        if inside && l.starts_with(b"## ") {
            out.extend_from_slice(&bullet);
            out.extend_from_slice(b"\n\n");
            inserted = true;
            inside = false;
        }
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    if inside && !inserted {
        out.extend_from_slice(&bullet);
        out.push(b'\n');
    }
    if !found {
        out.extend_from_slice(format!("\n{heading}\n\n").as_bytes());
        out.extend_from_slice(&bullet);
        out.push(b'\n');
    }
    if name == "Current State" && current_cap > 0 {
        out = prune_current(&out, current_cap);
    }
    touch(out, now)
}
fn prune_current(raw: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf: Vec<&[u8]> = Vec::new();
    let mut inside = false;
    fn flush(buf: &mut Vec<&[u8]>, out: &mut Vec<u8>, cap: usize) {
        let mut skip = buf
            .iter()
            .filter(|l| l.starts_with(b"- "))
            .count()
            .saturating_sub(cap);
        for l in buf.drain(..) {
            if skip > 0 && l.starts_with(b"- ") {
                skip -= 1;
            } else {
                out.extend_from_slice(l);
                out.push(b'\n');
            }
        }
    }
    for l in lines(raw) {
        if l == b"## Current State" {
            out.extend_from_slice(l);
            out.push(b'\n');
            inside = true;
            continue;
        }
        if inside && l.starts_with(b"## ") {
            flush(&mut buf, &mut out, cap);
            inside = false;
        }
        if inside {
            buf.push(l);
        } else {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
    }
    if inside {
        flush(&mut buf, &mut out, cap);
    }
    out
}
/// Default handoff packet mutation and stale TTL: 7 days (604800 seconds).
pub const DEFAULT_TTL_SECONDS: u64 = 604800;

/// Display-only dormancy threshold: 72 hours (in seconds).
pub const DORMANT_THRESHOLD_SECONDS: u64 = 72 * 3600;

/// Effective `OMS_AGENT_TASK_TTL` value from the environment (default 7 days).
pub fn configured_ttl() -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    std::env::var_os("OMS_AGENT_TASK_TTL")
        .filter(|v| !v.is_empty())
        .map_or_else(
            || DEFAULT_TTL_SECONDS.to_string().into_bytes(),
            |v| v.as_bytes().to_vec(),
        )
}

/// Non-negative idle age in seconds from `last_activity` (or `updated` fallback),
/// or `None` when absent, unparseable, or in the future relative to `now`.
pub fn idle_seconds(raw: &[u8], now: DateTime<Utc>) -> Option<u64> {
    let value = metadata(raw, "last_activity")
        .filter(|v| !v.is_empty())
        .or_else(|| metadata(raw, "updated").filter(|v| !v.is_empty()))?;
    let value = std::str::from_utf8(value).ok()?.trim();
    let then = chrono::DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%SZ")
                .ok()
                .map(|t| t.and_utc())
        })?;
    if now < then {
        return None;
    }
    u64::try_from(now.signed_duration_since(then).num_seconds()).ok()
}

/// Display freshness of a handoff packet relative to `OMS_AGENT_TASK_TTL` (stale)
/// and the 72-hour display-only dormancy threshold (dormant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketFreshness {
    pub idle_seconds: Option<u64>,
    pub stale: bool,
    pub dormant: bool,
}

impl PacketFreshness {
    pub fn idle_days(self) -> Option<u64> {
        self.idle_seconds.map(|s| s / 86400)
    }

    /// Display status label: `stale` when TTL says stale, else `dormant` when idle >= 72h.
    pub fn label(self) -> Option<String> {
        let word = if self.stale {
            "stale"
        } else if self.dormant {
            "dormant"
        } else {
            return None;
        };
        Some(match self.idle_days() {
            Some(days) => format!("{word} (idle {days}d)"),
            None => word.to_owned(),
        })
    }

    /// Hint body for stale/dormant packets; with `"  hint: "` prefix fits 80 columns.
    pub fn hint(self) -> Option<&'static str> {
        if self.stale {
            Some("stale handoff packet; run `oms agent-task close --reason '...'`")
        } else if self.dormant {
            Some("dormant handoff packet; run `oms agent-task close --reason '...'`")
        } else {
            None
        }
    }
}

/// Evaluate packet staleness (against `ttl`, defaulting to 7d) and display-only
/// dormancy (idle >= 72h when not stale).
pub fn freshness(raw: &[u8], ttl: &[u8], now: DateTime<Utc>) -> PacketFreshness {
    let idle_seconds = idle_seconds(raw, now);
    let closed = metadata(raw, "status") == Some(b"closed");
    let is_stale = !closed && stale(raw, ttl, now);
    let is_dormant =
        !closed && !is_stale && idle_seconds.is_some_and(|age| age >= DORMANT_THRESHOLD_SECONDS);
    PacketFreshness {
        idle_seconds,
        stale: is_stale,
        dormant: is_dormant,
    }
}

/// Evaluate packet freshness using `OMS_AGENT_TASK_TTL` from the environment.
pub fn freshness_from_env(raw: &[u8], now: DateTime<Utc>) -> PacketFreshness {
    freshness(raw, &configured_ttl(), now)
}

pub fn stale(raw: &[u8], ttl: &[u8], now: DateTime<Utc>) -> bool {
    let default_ttl;
    let ttl = if !ttl.is_empty() && ttl.iter().all(u8::is_ascii_digit) {
        std::str::from_utf8(ttl).unwrap()
    } else {
        default_ttl = DEFAULT_TTL_SECONDS.to_string();
        &default_ttl
    };
    let Some(age) = idle_seconds(raw, now) else {
        return false;
    };
    decimal_cmp(age.to_string().as_bytes(), ttl.as_bytes()) != std::cmp::Ordering::Less
}
pub fn decimal_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    fn zero(v: &[u8]) -> &[u8] {
        v.iter().position(|b| *b != b'0').map_or(b"0", |i| &v[i..])
    }
    let (a, b) = (zero(a), zero(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}
pub fn receipt(raw: &[u8], note: &[u8], key: &[u8]) -> Result<bool> {
    let mut records = BTreeMap::new();
    for l in lines(raw)
        .into_iter()
        .take_while(|l| !l.starts_with(b"## "))
    {
        if let Some(rest) = l.strip_prefix(b"- append_receipt_") {
            let Some(offset) = rest.iter().position(|b| *b == b':') else {
                return Err(Error::new(1, "invalid task append receipt metadata"));
            };
            let (k, v) = (&rest[..offset], &rest[offset + 1..]);
            if !crate::hex(k, 64, 64)
                || v.len() != 65
                || v[0] != b' '
                || !crate::hex(&v[1..], 64, 64)
                || records.insert(k, &v[1..]).is_some()
            {
                return Err(Error::new(1, "invalid task append receipt metadata"));
            }
        }
    }
    if records.len() > 256 {
        return Err(Error::new(1, "task append receipt limit exceeded"));
    }
    if let Some(previous) = records.get(key) {
        if *previous != hash(note).as_bytes() {
            return Err(Error::new(
                1,
                "task append dedupe key already binds another note",
            ));
        }
        return Ok(true);
    }
    if records.len() >= 256 {
        return Err(Error::new(
            1,
            "task append receipt limit reached; rotate the task",
        ));
    }
    Ok(false)
}
pub fn add_receipt(raw: &[u8], note: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let offset = raw
        .windows(4)
        .position(|w| w == b"\n## ")
        .ok_or_else(|| Error::new(1, "task append produced no section boundary"))?;
    let mut out = raw[..offset].to_vec();
    out.extend_from_slice(b"\n- append_receipt_");
    out.extend_from_slice(key);
    out.extend_from_slice(b": ");
    out.extend_from_slice(hash(note).as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&raw[offset..]);
    Ok(out)
}

/// Matches the section-aware hard byte cap, including the marker/newline and
/// original section order. Does not change the durable packet.
pub fn prune_context(raw: &[u8], budget: usize) -> Vec<u8> {
    const MARKER: &[u8] =
        b"... task context truncated; older entries dropped; newest entries retained ...";
    if budget == 0 {
        return Vec::new();
    }
    if MARKER.len() + 1 > budget {
        let mut out = MARKER[..budget - 1].to_vec();
        out.push(b'\n');
        return out;
    }
    let mut sections: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
    let mut current = None;
    for l in lines(raw) {
        if l.starts_with(b"## ") {
            let index = sections
                .iter()
                .position(|(h, _)| h == l)
                .unwrap_or_else(|| {
                    sections.push((l.to_vec(), Vec::new()));
                    sections.len() - 1
                });
            current = Some(index);
        } else if let Some(i) = current {
            sections[i].1.push(l.to_vec());
        }
    }
    let wanted = [
        "## Goal",
        "## Verify",
        "## Next Step",
        "## Current State",
        "## Decisions",
        "## Done Criteria",
        "## Constraints",
    ];
    let mut remaining = budget - MARKER.len() - 1;
    let mut kept = Vec::new();
    for h in wanted {
        if let Some(i) = sections.iter().position(|(name, _)| name == h.as_bytes()) {
            let cost = h.len() + 2;
            if cost <= remaining {
                remaining -= cost;
                kept.push(i);
            }
        }
    }
    let quota = if kept.is_empty() {
        0
    } else {
        remaining / kept.len()
    };
    let mut chosen = BTreeMap::new();
    for &i in &kept {
        let (h, body) = &sections[i];
        let mut allowance = quota;
        let mut has_content = false;
        let mut order: Vec<_> = (0..body.len()).collect();
        if [
            b"## Verify".as_slice(),
            b"## Decisions",
            b"## Current State",
            b"## Next Step",
        ]
        .contains(&h.as_slice())
        {
            order.reverse();
        }
        for j in order {
            let l = &body[j];
            if l.is_empty() {
                continue;
            }
            let candidate = if l.len() < allowance {
                Some(l.as_slice())
            } else if !has_content && b"(... entry clipped ...)".len() < allowance {
                Some(b"(... entry clipped ...)".as_slice())
            } else {
                None
            };
            if let Some(c) = candidate {
                allowance -= c.len() + 1;
                chosen.insert((i, j), c.to_vec());
                has_content = true;
            }
        }
    }
    let mut out = Vec::new();
    for (i, (h, body)) in sections.iter().enumerate() {
        if !kept.contains(&i) {
            continue;
        }
        out.extend_from_slice(h);
        out.extend_from_slice(b"\n\n");
        for j in 0..body.len() {
            if let Some(l) = chosen.get(&(i, j)) {
                out.extend_from_slice(l);
                out.push(b'\n');
            }
        }
    }
    out.extend_from_slice(MARKER);
    out.push(b'\n');
    out
}
