//! Read-only room lineage, projection and identity binding for queued inputs.
//!
//! Mirrors the reader half of `scripts/lib/room.py`; appends go through `HostServices`.
use crate::json::{J, array_of, dup, get, str_of, string};
use crate::{Result, UNSAFE, read};
use oms_safe_fs::Root;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_ROW: usize = 65536;
const TAIL_BYTES: usize = 8192;
const MAX_HOPS: usize = 64;
const MAX_LINEAGE: u64 = 32 * 1024 * 1024;

pub type Rows = Vec<J>;
pub type History = Vec<(String, Rows)>;

/// `thread_live.ID` plus the leading-character rule of `room.identifier`.
pub fn valid_room(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 160
        && (b[0].is_ascii_alphanumeric() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
}
fn thread_bytes(root: &Root, room: &str) -> Result<Vec<u8>> {
    if !valid_thread(room) {
        return Err(UNSAFE);
    }
    read(
        root,
        &PathBuf::from(".oms/threads").join(format!("{room}.jsonl")),
        MAX_FILE,
    )
    .map_err(|_| UNSAFE)
}
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let mut result = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < data.len() {
        if data[i] == b'\n' || data[i] == b'\r' {
            result.push(&data[start..i]);
            if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    if start < data.len() {
        result.push(&data[start..]);
    }
    result
}
fn decode(data: &[u8], room: &str) -> Result<Rows> {
    if data.len() > MAX_FILE || (!data.is_empty() && !data.ends_with(b"\n")) {
        return Err(UNSAFE);
    }
    let mut rows = Vec::new();
    for line in lines(data) {
        if line.len() > MAX_ROW {
            return Err(UNSAFE);
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let row = crate::json::parse(line)?;
        let ok = matches!(row, J::Object(_))
            && str_of(&row, "thread") == Some(room)
            && crate::json::int_of(&row, "seq").is_some()
            && get(&row, "room_event").is_none_or(|e| matches!(e, J::Object(_)));
        if !ok {
            return Err(UNSAFE);
        }
        rows.push(row);
    }
    Ok(rows)
}
pub fn records(root: &Root, room: &str) -> Result<Rows> {
    decode(&thread_bytes(root, room)?, room)
}
fn event_of(row: &J) -> Option<&J> {
    get(row, "room_event")
}
fn kind_of(row: &J) -> &str {
    event_of(row).and_then(|e| str_of(e, "kind")).unwrap_or("")
}

/// The successor named by a sealed log's last row, read from at most `TAIL_BYTES`.
fn tail_successor(data: &[u8], room: &str) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let offset = data.len().saturating_sub(TAIL_BYTES);
    let chunk = &data[offset..];
    if !chunk.ends_with(b"\n") {
        return None;
    }
    let start = chunk[..chunk.len() - 1].iter().rposition(|b| *b == b'\n');
    if start.is_none() && offset > 0 {
        return None;
    }
    let row = crate::json::parse(&chunk[start.map_or(0, |s| s + 1)..]).ok()?;
    let event = event_of(&row)?;
    let target = str_of(event, "target")?;
    (str_of(&row, "thread") == Some(room)
        && str_of(event, "kind") == Some("successor")
        && valid_thread(target))
    .then(|| target.to_owned())
}
fn valid_thread(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 160
        && (b[0].is_ascii_alphanumeric() || b[0] == b'_' || b[0] == b'-')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
}
fn predecessor(root: &Root, room: &str) -> Option<String> {
    let data = thread_bytes(root, room).ok()?;
    let mut head = Vec::new();
    for chunk in data.split_inclusive(|b| *b == b'\n').take(2) {
        if chunk.len() > MAX_ROW || !chunk.ends_with(b"\n") {
            return None;
        }
        head.extend_from_slice(chunk);
    }
    let rows = decode(&head, room).ok()?;
    let event = (rows.len() == 2).then(|| event_of(&rows[1])).flatten()?;
    if str_of(event, "kind") == Some("created") {
        str_of(event, "predecessor").map(str::to_owned)
    } else {
        None
    }
}
fn successor_id(old: &str) -> String {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("successor:{old}").as_bytes())
    );
    format!("room-{}", &digest[..12])
}

/// The room an id now names: follow sealed logs to the successor that names them back.
pub fn resolve(root: &Root, room: &str) -> Result<String> {
    if !valid_thread(room) {
        return Ok(room.to_owned());
    }
    let mut current = room.to_owned();
    let mut seen = vec![current.clone()];
    for _ in 0..MAX_HOPS {
        let Ok(data) = thread_bytes(root, &current) else {
            return Ok(current);
        };
        let mut next = tail_successor(&data, &current);
        if let Some(candidate) = &next
            && (seen.contains(candidate)
                || predecessor(root, candidate).as_deref() != Some(&current))
        {
            next = None;
        }
        let Some(next) = next else {
            return Ok(current);
        };
        seen.push(next.clone());
        current = next;
    }
    Err(UNSAFE)
}
pub fn same_room(root: &Root, a: &str, b: &str) -> Result<bool> {
    Ok(a == b || (!a.is_empty() && !b.is_empty() && resolve(root, a)? == resolve(root, b)?))
}
fn carried(root: &Root, room: &str) -> Result<bool> {
    Ok(project(&records(root, room)?)?.carried)
}
/// Only mutually linked, sealed predecessors, nearest first.
fn sealed_predecessors(root: &Root, room: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut current = room.to_owned();
    let mut seen = vec![current.clone()];
    for _ in 0..MAX_HOPS {
        let Some(old) = predecessor(root, &current) else {
            return Ok(result);
        };
        let sealed = thread_bytes(root, &old)
            .ok()
            .and_then(|data| tail_successor(&data, &old));
        let Some(sealed) = sealed else {
            if carried(root, &current)? {
                return Err(UNSAFE);
            }
            return Ok(result);
        };
        if seen.contains(&old) || successor_id(&old) != current || sealed != current {
            return Err(UNSAFE);
        }
        result.push(old.clone());
        seen.push(old.clone());
        current = old;
    }
    if predecessor(root, &current).is_some() {
        return Err(UNSAFE);
    }
    Ok(result)
}
pub fn history(root: &Root, room: &str) -> Result<History> {
    let selected = resolve(root, room)?;
    let mut names: Vec<String> = sealed_predecessors(root, &selected)?;
    names.reverse();
    names.push(selected);
    let mut result = Vec::new();
    let mut total = 0u64;
    for name in names {
        let data = thread_bytes(root, &name)?;
        total += data.len() as u64;
        if total > MAX_LINEAGE {
            return Err("room lineage exceeds the 32 MiB reconciliation bound");
        }
        let rows = decode(&data, &name)?;
        result.push((name, rows));
    }
    Ok(result)
}

fn shape(event: &J) -> Result<()> {
    let J::Object(_) = event else {
        return Err(UNSAFE);
    };
    let kind = str_of(event, "kind");
    let required: &[&str] = match kind {
        Some("created" | "carried") => &[],
        Some("join") => &["participant", "provider", "role", "label"],
        Some("bind") => &["participant", "consumer"],
        Some("main-swap") => &["participant", "provider", "model", "attempt", "harness"],
        Some("scope" | "leave" | "closed" | "received") => &["participant"],
        Some("describe") => &["participant", "label"],
        Some("message") => &["id", "sender", "recipient", "message_kind"],
        Some("publication") => &["id", "digest", "status"],
        Some("successor") => &["participant", "target"],
        Some("call-result") => &[
            "event_id",
            "task_id",
            "call_id",
            "attempt_id",
            "role",
            "access",
            "verify_status",
        ],
        Some("review-dispatch" | "review-dispatch-failed") => &[
            "event_id",
            "task_id",
            "call_id",
            "patch_sha256",
            "review_call_id",
        ],
        _ => return Err(UNSAFE),
    };
    if required.iter().any(|key| str_of(event, key).is_none()) {
        return Err(UNSAFE);
    }
    let J::Object(fields) = event else {
        return Err(UNSAFE);
    };
    for (key, value) in fields {
        let key = std::str::from_utf8(key).map_err(|_| UNSAFE)?;
        let text_or_null = matches!(value, J::String(_) | J::Null);
        let list =
            matches!(value, J::Array(items) if items.iter().all(|i| matches!(i, J::String(_))));
        let bad = match key {
            "model" | "consumer" | "previous" | "parent" | "reply_to" | "task_id" | "turn_id"
            | "target" | "participant" | "harness" | "predecessor" => !text_or_null,
            "owns" | "messages" | "audience" => !list,
            _ => false,
        };
        if bad {
            return Err(UNSAFE);
        }
    }
    let boolean = |key: &str| matches!(get(event, key), Some(J::Bool(_)));
    match kind {
        Some("scope") if get(event, "owns").is_none() => return Err(UNSAFE),
        Some("received") if get(event, "messages").is_none() => return Err(UNSAFE),
        Some("publication") if !boolean("delivery_unknown") => return Err(UNSAFE),
        Some("publication") if get(event, "target").is_some_and(|v| !matches!(v, J::String(_))) => {
            return Err(UNSAFE);
        }
        Some("message") if get(event, "quiet").is_some() && !boolean("quiet") => {
            return Err(UNSAFE);
        }
        Some("main-swap") if !matches!(get(event, "attempt"), Some(J::String(_))) => {
            return Err(UNSAFE);
        }
        Some("join") => {
            if let Some(alias) = get(event, "alias").filter(|v| **v != J::Null) {
                let ok = matches!(alias, J::String(b) if (1..=8).contains(&b.len())
                    && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-'));
                if !ok {
                    return Err(UNSAFE);
                }
            }
            if let Some(role) = get(event, "dispatch_role").filter(|v| **v != J::Null) {
                let ok = matches!(role, J::String(b) if (1..=64).contains(&b.len())
                    && b[0].is_ascii_lowercase()
                    && b[1..].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-'));
                if !ok {
                    return Err(UNSAFE);
                }
            }
            if get(event, "purpose").is_some_and(|v| !matches!(v, J::String(_) | J::Null)) {
                return Err(UNSAFE);
            }
            if get(event, "state").is_some_and(|v| *v != J::Null && *v != string("queued")) {
                return Err(UNSAFE);
            }
        }
        Some("describe") => {
            let J::Object(fields) = event else {
                return Err(UNSAFE);
            };
            let label = str_of(event, "label").unwrap_or("");
            if fields.len() != 3 || label.is_empty() || label.chars().count() > 120 {
                return Err(UNSAFE);
            }
        }
        Some("call-result") => {
            let int = |key: &str| matches!(get(event, key), Some(J::Number(n)) if !n.contains(['.', 'e', 'E']));
            let ok = get(event, "exit").is_some()
                && get(event, "needs_review").is_some()
                && matches!(
                    str_of(event, "role"),
                    Some("main" | "worker" | "advisor" | "reviewer" | "researcher")
                )
                && matches!(str_of(event, "access"), Some("read" | "write"))
                && int("exit")
                && boolean("needs_review")
                && matches!(
                    str_of(event, "verify_status"),
                    Some("passed" | "failed" | "not_run")
                )
                && get(event, "verify_exit").is_none_or(|v| *v == J::Null || int("verify_exit"))
                && get(event, "changed_files").is_none_or(
                    |v| matches!(v, J::Array(a) if a.iter().all(|i| matches!(i, J::String(_)))),
                );
            if !ok {
                return Err(UNSAFE);
            }
        }
        Some("review-dispatch" | "review-dispatch-failed") => {
            let digest = str_of(event, "patch_sha256").unwrap_or("");
            let attempt = crate::json::int_of(event, "attempt");
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                || attempt.is_none_or(|n| n < 1)
            {
                return Err(UNSAFE);
            }
        }
        _ => {}
    }
    Ok(())
}

pub struct Member {
    pub name: String,
    pub joined: bool,
    pub fields: J,
}
pub struct Projection {
    pub task_id: J,
    pub closed: bool,
    pub successor: Option<String>,
    pub predecessor: Option<String>,
    pub carried: bool,
    pub owner: Option<String>,
    pub members: Vec<Member>,
}
fn member<'a>(members: &'a mut [Member], name: &str) -> Result<&'a mut Member> {
    members.iter_mut().find(|m| m.name == name).ok_or(UNSAFE)
}
pub fn project(rows: &[J]) -> Result<Projection> {
    for row in rows {
        if let Some(event) = event_of(row)
            && let J::Object(f) = event
            && !f.is_empty()
        {
            shape(event)?;
        }
        if get(row, "text").is_some_and(|t| !matches!(t, J::String(_))) {
            return Err(UNSAFE);
        }
    }
    let created = rows
        .iter()
        .find(|row| kind_of(row) == "created")
        .ok_or(UNSAFE)?;
    let created_event = event_of(created).ok_or(UNSAFE)?;
    let mut result = Projection {
        task_id: get(created_event, "task_id").map_or(J::Null, dup),
        closed: false,
        successor: None,
        predecessor: str_of(created_event, "predecessor").map(str::to_owned),
        carried: false,
        owner: None,
        members: Vec::new(),
    };
    for row in rows {
        if str_of(row, "role") == Some("closed") {
            result.closed = true;
        }
        let Some(event) = event_of(row) else {
            continue;
        };
        let name = str_of(event, "participant").unwrap_or("");
        match kind_of(row) {
            "closed" => result.closed = true,
            "join" => {
                let mut fields = dup(event);
                crate::json::set(&mut fields, "joined", J::Bool(true));
                let entry = Member {
                    name: name.to_owned(),
                    joined: true,
                    fields,
                };
                if let Some(slot) = result.members.iter_mut().find(|m| m.name == name) {
                    *slot = entry;
                } else {
                    result.members.push(entry);
                }
                if result.owner.is_none() && str_of(event, "role") == Some("main") {
                    result.owner = Some(name.to_owned());
                }
            }
            "main-swap" => {
                let m = member(&mut result.members, name)?;
                for key in ["provider", "model"] {
                    crate::json::set(&mut m.fields, key, dup(get(event, key).ok_or(UNSAFE)?));
                }
                crate::json::set(&mut m.fields, "consumer", J::Null);
            }
            "leave" => {
                if let Some(m) = result.members.iter_mut().find(|m| m.name == name) {
                    m.joined = false;
                }
            }
            "describe" => {
                let m = member(&mut result.members, name)?;
                if !m.joined || str_of(&m.fields, "role") != Some("main") {
                    return Err(UNSAFE);
                }
                crate::json::set(
                    &mut m.fields,
                    "label",
                    dup(get(event, "label").ok_or(UNSAFE)?),
                );
            }
            "bind" => {
                let m = member(&mut result.members, name)?;
                crate::json::set(
                    &mut m.fields,
                    "consumer",
                    dup(get(event, "consumer").ok_or(UNSAFE)?),
                );
            }
            "scope" => {
                let m = member(&mut result.members, name)?;
                crate::json::set(
                    &mut m.fields,
                    "owns",
                    dup(get(event, "owns").ok_or(UNSAFE)?),
                );
            }
            "successor" => result.successor = str_of(event, "target").map(str::to_owned),
            "carried" => result.carried = true,
            _ => {}
        }
    }
    Ok(result)
}
pub fn participant<'a>(state: &'a Projection, value: &str) -> Result<&'a Member> {
    if !valid_room(value) {
        return Err(UNSAFE);
    }
    state
        .members
        .iter()
        .find(|m| m.name == value && m.joined)
        .ok_or(UNSAFE)
}

/// Carried copies preserve evidence but cannot mint a new enrollment epoch.
pub fn ordinary<'a>(histories: &[(&'a str, &'a [J])]) -> Vec<(&'a str, &'a J)> {
    let mut result = Vec::new();
    for (name, rows) in histories {
        let mut carried = rows.len() > 1
            && event_of(&rows[1])
                .is_some_and(|e| str_of(e, "predecessor").is_some_and(|p| !p.is_empty()));
        for row in *rows {
            if kind_of(row) == "carried" {
                carried = false;
                continue;
            }
            if !carried {
                result.push((*name, row));
            }
        }
    }
    result
}
fn view(history: &History) -> Vec<(&str, &[J])> {
    history
        .iter()
        .map(|(name, rows)| (name.as_str(), rows.as_slice()))
        .collect()
}
pub fn identity(histories: &[(&str, &[J])], who: &str, to: &str) -> Result<J> {
    let last = histories.last().ok_or(UNSAFE)?;
    let current = project(last.1)?;
    if current.closed
        || current.successor.is_some()
        || (current.predecessor.is_some() && !current.carried)
    {
        return Err("input target room is closed, sealed, or incompletely carried");
    }
    if who == to || to == "all" {
        return Err("inputs require distinct explicit sender and recipient participants");
    }
    let sender = participant(&current, who)?;
    let recipient = participant(&current, to)?;
    let owner = participant(&current, current.owner.as_deref().ok_or(UNSAFE)?)?;
    let mut epochs: BTreeMap<&str, (&str, i64)> = BTreeMap::new();
    for (name, row) in ordinary(histories) {
        if let Some(event) = event_of(row)
            && matches!(
                kind_of(row),
                "join" | "leave" | "bind" | "main-swap" | "scope"
            )
            && let Some(p) = str_of(event, "participant")
        {
            epochs.insert(p, (name, crate::json::int_of(row, "seq").unwrap_or(0)));
        }
    }
    let signature = |m: &Member| -> Result<J> {
        let mut out = Vec::new();
        for key in [
            "participant",
            "provider",
            "role",
            "consumer",
            "parent",
            "task_id",
            "owns",
        ] {
            out.push((
                key.as_bytes().to_vec(),
                get(&m.fields, key).map_or(J::Null, dup),
            ));
        }
        let (room, seq) = epochs
            .get(m.name.as_str())
            .ok_or("participant enrollment provenance is unavailable")?;
        out.push((
            b"epoch".to_vec(),
            crate::json::object([("room", string(room)), ("seq", crate::json::number(*seq))]),
        ));
        Ok(J::Object(out))
    };
    Ok(crate::json::object([
        ("root_room", string(histories[0].0)),
        ("task_id", dup(&current.task_id)),
        ("sender", signature(sender)?),
        ("recipient", signature(recipient)?),
        ("owner", signature(owner)?),
    ]))
}
pub fn binding(root: &Root, room: &str, who: &str, to: &str) -> Result<J> {
    let h = history(root, room)?;
    let found = identity(&view(&h), who, to)?;
    Ok(crate::json::object([
        ("room", string(room)),
        ("resolved_room", string(&h[h.len() - 1].0)),
        ("from", string(who)),
        ("to", string(to)),
        ("identity", found),
    ]))
}
/// Message rows for one id across the lineage, plus acknowledgments by the recipient.
pub struct Delivery {
    pub room: String,
    pub seq: i64,
    pub ts: J,
}
pub struct Found {
    pub original: Option<Delivery>,
    pub acknowledgments: Vec<(String, i64)>,
}
/// Locate the one original delivery of `message_id`; refuse conflicting or duplicate evidence.
pub fn find_delivery(
    h: &History,
    message_id: &str,
    from: &str,
    to: &str,
    text: &str,
    expected_identity: &J,
) -> Result<Found> {
    let all = view(h);
    let mut matches = Vec::new();
    for (index, (name, rows)) in all.iter().enumerate() {
        let ordinary_rows: Vec<(&str, &J)> = ordinary(&[(*name, *rows)]);
        for (offset, row) in rows.iter().enumerate() {
            let Some(event) = event_of(row).filter(|e| {
                str_of(e, "kind") == Some("message") && str_of(e, "id") == Some(message_id)
            }) else {
                continue;
            };
            if str_of(event, "sender") != Some(from)
                || str_of(event, "recipient") != Some(to)
                || str_of(event, "message_kind") != Some("note")
                || str_of(row, "text") != Some(text)
            {
                return Err(
                    "message ID exists with different content or addressing; delivery remains uncertain",
                );
            }
            if !ordinary_rows.iter().any(|(_, r)| std::ptr::eq(*r, row)) {
                continue;
            }
            let mut historical: Vec<(&str, &[J])> = all[..index].to_vec();
            historical.push((*name, &rows[..=offset]));
            if !crate::json::same(&identity(&historical, from, to)?, expected_identity) {
                return Err(
                    "message was appended under a different participant or room-owner identity",
                );
            }
            matches.push(Delivery {
                room: (*name).to_owned(),
                seq: crate::json::int_of(row, "seq").unwrap_or(0),
                ts: get(row, "ts").map_or(J::Null, dup),
            });
        }
    }
    if matches.len() > 1 {
        return Err("message ID has multiple original room deliveries");
    }
    let acknowledgments = ordinary(&all)
        .into_iter()
        .filter(|(_, row)| {
            kind_of(row) == "received"
                && event_of(row).is_some_and(|e| {
                    str_of(e, "participant") == Some(to)
                        && array_of(e, "messages").is_some_and(|m| m.contains(&string(message_id)))
                })
        })
        .map(|(name, row)| {
            (
                name.to_owned(),
                crate::json::int_of(row, "seq").unwrap_or(0),
            )
        })
        .collect();
    Ok(Found {
        original: matches.pop(),
        acknowledgments,
    })
}
