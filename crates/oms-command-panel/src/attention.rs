//! Presentation-only read markers. Opening never acknowledges a room message.
use crate::{Error, Result, rows, text};
use oms_panel_core::{Store, Value};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

pub const LIMIT: usize = 12;
pub const CLASSES: &[&str] = &[
    "needs",
    "failed",
    "guard_stop",
    "timed_out",
    "verify_failed",
    "verify_timed_out",
    "provider_error",
    "decide",
    "stale",
    "mail",
    "info",
];
const READ_PATH: &str = ".oms/artifacts/panel-attention-read.json";
#[derive(Clone, Debug, serde::Serialize)]
pub struct Item {
    pub row_id: (String, String),
    pub cls: String,
    pub stamp: String,
    pub who: String,
    pub what: String,
    pub task: Option<String>,
    pub unread: bool,
}
pub fn row_key(row: &(String, String)) -> String {
    fn quoted(value: &str) -> String {
        let quote = if value.contains('\'') && !value.contains('"') {
            '"'
        } else {
            '\''
        };
        let mut out = String::from(quote);
        for c in value.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out.push(quote);
        out
    }
    oms_panel_core::digest(format!("({}, {})", quoted(&row.0), quoted(&row.1)).as_bytes())[..16]
        .into()
}
fn room_key(room: &str) -> &str {
    if !room.is_empty()
        && room.len() <= 64
        && (room.as_bytes()[0].is_ascii_lowercase() || room.as_bytes()[0].is_ascii_digit())
        && room
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
    {
        room
    } else {
        "default"
    }
}
fn parse(value: &Value) -> BTreeMap<String, BTreeMap<String, (String, i64)>> {
    let mut rooms = BTreeMap::new();
    if let Some(raw) = value["rooms"].as_object() {
        for (name, rows) in raw {
            let mut saved = BTreeMap::new();
            if let Some(rows) = rows.as_object() {
                for (key, entry) in rows {
                    if key.len() == 16
                        && key
                            .bytes()
                            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                        && entry.as_array().is_some_and(|v| v.len() == 2)
                        && let (Some(class), Some(at)) = (entry[0].as_str(), entry[1].as_i64())
                        && !class.is_empty()
                        && class.len() <= 24
                        && class.bytes().all(|v| v.is_ascii_lowercase() || v == b'_')
                    {
                        saved.insert(key.clone(), (class.into(), at));
                    }
                }
            }
            rooms.insert(name.clone(), saved);
        }
    }
    rooms
}
pub fn load(repo: &Path, room: &str) -> BTreeMap<String, String> {
    let value = Store::open(repo)
        .and_then(|s| s.json(Path::new(READ_PATH), 65536, true))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    parse(&value)
        .remove(room_key(room))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, (v, _))| (k, v))
        .collect()
}
pub fn items(report: &Value, read: &BTreeMap<String, String>) -> Vec<Item> {
    let mut items = Vec::new();
    let calls = crate::render::calls(report);
    let mut failing = BTreeSet::new();
    for call in &calls {
        let state = text(call, "state");
        let ending = text(call, "ending");
        let task = text(call, "task_id");
        let finalized = report["finalized"][task]
            .as_str()
            .is_some_and(|v| ["completed", "accepted", "failed", "rejected"].contains(&v));
        if state == "cancelled"
            || finalized
            || call["returned_at"].is_string()
            || call["room_only_history"] == true
            || call["room_only_stale"] == true
        {
            continue;
        }
        let class = if CLASSES[2..7].contains(&ending) {
            ending
        } else if ["failed", "timed_out", "abandoned", "orphaned"].contains(&state) {
            "failed"
        } else if ["waiting_input", "waiting_approval", "blocked"].contains(&state) {
            "needs"
        } else if state == "review"
            || (state == "done"
                && rows(&report["awaiting_admission"])
                    .iter()
                    .any(|v| v == &call["participant"]))
        {
            "decide"
        } else if call["lifecycle_stale"] == true {
            "stale"
        } else {
            continue;
        };
        if [
            "failed",
            "guard_stop",
            "timed_out",
            "verify_failed",
            "verify_timed_out",
            "provider_error",
        ]
        .contains(&class)
        {
            failing.insert(task.to_owned());
        }
        let what = match class {
            "needs" if state == "waiting_approval" => "waiting for approval",
            "needs" if state == "waiting_input" => "waiting for input",
            "decide" => "awaiting decision",
            "stale" => "no recent lifecycle events",
            "needs" => "blocked",
            other => other,
        };
        let model = text(call, "model");
        let role = text(call, "role");
        let alias = text(call, "alias");
        let who = match (model.is_empty(), role.is_empty()) {
            (false, false) => format!("{model} {role}"),
            (true, false) => {
                if alias.is_empty() {
                    role.into()
                } else {
                    format!("{alias} {role}")
                }
            }
            (false, true) => model.into(),
            (true, true) => [
                alias,
                text(call, "tool"),
                text(call, "provider"),
                text(call, "participant"),
                text(call, "attempt"),
            ]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or("call")
            .into(),
        };
        let base_what = what.replace('_', " ");
        let detail = [
            text(call, "title"),
            text(call, "task_id"),
            text(call, "ending"),
        ]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or("");
        let summary = if detail.is_empty() || detail == base_what {
            base_what
        } else {
            format!("{base_what}: {detail}")
        };
        items.push(Item {
            row_id: ("call".into(), text(call, "participant").into()),
            cls: class.into(),
            stamp: text(call, "updated_at").into(),
            who,
            what: summary,
            task: (!task.is_empty()).then(|| task.into()),
            unread: true,
        });
    }
    for message in rows(&report["room"]["messages"]) {
        if message["answered"] == true || rows(&message["pending_for"]).is_empty() {
            continue;
        }
        let id = text(message, "id");
        if id.is_empty() {
            continue;
        }
        items.push(Item {
            row_id: ("message".into(), id.into()),
            cls: if message["message_kind"] == "question" {
                "needs"
            } else {
                "mail"
            }
            .into(),
            stamp: text(message, "ts").into(),
            who: text(message, "sender").into(),
            what: crate::render::clean(text(message, "text"), 80),
            task: None,
            unread: true,
        });
    }
    let tasks: Vec<_> = if let Some(v) = report["plan"]["tasks"].as_object() {
        v.values().collect()
    } else {
        rows(&report["plan"]["tasks"]).iter().collect()
    };
    for task in tasks {
        let id = text(task, "id");
        if task["status"] == "held" && !failing.contains(id) {
            let workers: Vec<_> = calls.iter().filter(|v| v["task_id"] == id).collect();
            if !workers.is_empty()
                && workers
                    .iter()
                    .all(|v| oms_panel_core::state::TERMINAL_STATES.contains(&text(v, "state")))
            {
                items.push(Item {
                    row_id: ("task".into(), id.into()),
                    cls: "stale".into(),
                    stamp: text(task, "claimed_at").into(),
                    who: format!("Task {id}"),
                    what: "workers ended, still held".into(),
                    task: Some(id.into()),
                    unread: true,
                });
            }
        }
    }
    items.sort_by(|a, b| {
        let rank = |s: &str| {
            CLASSES
                .iter()
                .position(|v| *v == s)
                .unwrap_or(CLASSES.len())
        };
        rank(&a.cls).cmp(&rank(&b.cls)).then_with(|| {
            if a.cls == "decide" {
                a.stamp.cmp(&b.stamp)
            } else {
                b.stamp.cmp(&a.stamp)
            }
        })
    });
    let mut seen = BTreeSet::new();
    items.retain(|v| seen.insert(v.row_id.clone()));
    for item in &mut items {
        item.unread = read.get(&row_key(&item.row_id)) != Some(&item.cls);
    }
    items
}
pub fn mark(repo: &Path, room: &str, report: &Value, entries: &[Item]) -> Result<()> {
    if report["collection"]["ok"] != true
        || report["room"].get("error").is_some_and(|v| !v.is_null())
    {
        return Ok(());
    }
    let current: BTreeMap<_, _> = items(report, &BTreeMap::new())
        .into_iter()
        .map(|v| (row_key(&v.row_id), v.cls))
        .collect();
    let store = Store::open(repo)?;
    let tx = store.transaction("panel-attention-read", Duration::from_millis(100))?;
    let raw = tx
        .json(Path::new(READ_PATH), 65536, true)?
        .unwrap_or(Value::Null);
    let mut rooms = parse(&raw);
    let name = room_key(room).to_owned();
    let mut saved = rooms.remove(&name).unwrap_or_default();
    saved.retain(|key, (class, _)| current.get(key) == Some(class));
    let at = oms_panel_core::timestamp() as i64;
    for entry in entries {
        let key = row_key(&entry.row_id);
        if current.get(&key) == Some(&entry.cls) {
            saved.insert(key, (entry.cls.clone(), at));
        }
    }
    let mut ordered: Vec<_> = saved.into_iter().collect();
    ordered.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(&b.0)));
    let saved: BTreeMap<_, _> = ordered.into_iter().take(100).collect();
    if !saved.is_empty() {
        rooms.insert(name, saved);
    }
    let mut rooms: Vec<_> = rooms.into_iter().collect();
    rooms.sort_by_key(|(_, rows)| std::cmp::Reverse(rows.values().map(|v| v.1).max().unwrap_or(0)));
    let rooms: BTreeMap<_, _> = rooms.into_iter().take(8).collect();
    tx.write_json(
        Path::new(READ_PATH),
        &json!({"schema":1,"rooms":rooms}),
        false,
        65536,
    )
    .map_err(|e| Error {
        publication_uncertain: e.publication_uncertain,
        ..e
    })?;
    Ok(())
}
