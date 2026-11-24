//! Read-only desktop projections for the Board's Room history and recorded capacity.
use crate::{Error, Result, Services, commands::query};
use oms_command_room as room;
use oms_panel_core::{NativeCommand, Value};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool};

/// The request identity comes from the host. Project or ambient Room state
/// cannot substitute for an enrolled, unique, nonterminal Codex Main.
pub(super) fn bound_room(
    repo: &Path,
    digest: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Option<String>> {
    crate::parent_only()?;
    let Some(digest) = digest else {
        return Ok(None);
    };
    let events = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let reader = room::Room::open(
        repo,
        Default::default(),
        &room::LocalServices,
        stop,
        false,
        false,
    )
    .map_err(|_| Error::new("Room unavailable"))?;
    bound_room_from_events(
        &reader,
        digest,
        events
            .as_array()
            .ok_or_else(|| Error::new("invalid lifecycle attempts"))?,
    )
}

pub(super) fn bound_room_from_events(
    reader: &room::Room<'_>,
    digest: &str,
    events: &[Value],
) -> Result<Option<String>> {
    let attempts = events
        .iter()
        .filter(|r| r["tool"] == "panel-main")
        .map(|r| {
            let refs = &r["refs"];
            room::ConversationPanelAttempt {
                room: refs["panel_room_id"].as_str().unwrap_or_default(),
                participant: refs["panel_room_participant"]
                    .as_str()
                    .or_else(|| r["attempt_id"].as_str())
                    .unwrap_or_default(),
                terminal: r["terminal"].as_bool().unwrap_or(true),
            }
        });
    let result = reader
        .conversation_main(digest, attempts)
        .map_err(|_| Error::new("Room lookup unavailable"))?;
    Ok(result["conversationMain"]["room"]
        .as_str()
        .filter(|_| result["conversationBinding"].as_str() == Some("matched"))
        .map(str::to_owned))
}

/// Recheck the exact request binding on the same snapshot that supplies the rows.
pub(super) fn confirms_room(report: &Value, id: &str, digest: &str) -> bool {
    let room = &report["room"];
    report["collection"]["ok"] == true
        && room["id"] == id
        && room["closed"] == false
        && room["sealed"] != true
        && room["successor"].is_null()
        && room["participants"].as_array().is_some_and(|members| {
            members.iter().any(|m| {
                m["joined"] == true
                    && m["role"] == "main"
                    && m["provider"] == "codex"
                    && m["participant"] == room["owner"]
                    && room["owner"].is_string()
                    && m["consumer"] == digest
            })
        })
}

pub(super) fn visible(message: &Value, owner: &str) -> bool {
    message["sender"] == owner
        || message["recipient"] == owner
        || (message["recipient"] == "all"
            && message["targets"]
                .as_array()
                .is_some_and(|targets| targets.iter().any(|v| v == owner)))
}

pub(super) fn in_room(a: &Value, id: &str) -> bool {
    let refs = &a["refs"];
    let rooms = [refs["panel_room_id"].as_str(), refs["panel_room"].as_str()];
    rooms.contains(&Some(id)) && rooms.iter().flatten().all(|v| v.is_empty() || *v == id)
}

const PAGE_SIZE: usize = 12;
fn cell(v: &Value) -> Value {
    v.as_str()
        .map(|s| oms_panel_core::eventlog::clean(s, 140))
        .filter(|s| !s.is_empty())
        .map(Value::String)
        .unwrap_or(Value::Null)
}

// The native council dock follows a Room-bound panel-council attempt into
// this exact thread. Thread records contain no Room recipients; only bounded
// position counts and model identifiers may be projected from them.
fn council_metadata(repo: &Path, thread: &str) -> Option<(usize, String, Option<bool>)> {
    let suffix = thread.strip_prefix("panel-council-")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let store = oms_panel_core::Store::open(repo).ok()?;
    let bytes = store
        .read(
            Path::new(&format!(".oms/threads/{thread}.jsonl")),
            2 * 1024 * 1024,
            true,
        )
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    let mut count = 0;
    let mut models = std::collections::BTreeSet::new();
    let mut synthesis = None;
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let row: Value = serde_json::from_slice(line).ok()?;
        if row["thread"] != thread {
            return None;
        }
        if row["role"] == "answer" && row["text"].as_str().is_some_and(|s| !s.trim().is_empty()) {
            count += 1;
            if let Some(model) = row["model"].as_str().filter(|s| {
                !s.is_empty()
                    && s.len() <= 96
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            }) && models.len() < 6
            {
                models.insert(model.to_owned());
            }
        }
        if row["role"] == "synthesis" && row["text"].as_str().is_some_and(|s| !s.trim().is_empty())
        {
            synthesis = Some(true)
        }
    }
    Some((
        count,
        models.into_iter().collect::<Vec<_>>().join(", "),
        synthesis,
    ))
}

pub(super) fn page(
    repo: &Path,
    report: &Value,
    room: Option<&str>,
    view: &str,
    page: usize,
    stop: &AtomicBool,
) -> Result<Value> {
    let mut council_threads = BTreeMap::new();
    let mut rows = vec![];
    let mut status = "ready";
    let columns: &[(&str, &str)] = match view {
        "capacity" => &[
            ("metric", "항목"),
            ("used", "점유"),
            ("limit", "정책 한도"),
            ("active", "정책 활성"),
        ],
        "past" => &[
            ("at", "갱신 시각"),
            ("attempt", "실행"),
            ("provider", "공급자"),
            ("task", "작업"),
            ("state", "상태"),
        ],
        "debate" => &[
            ("at", "갱신 시각"),
            ("attempt", "토론 실행"),
            ("task", "작업"),
            ("state", "상태"),
            ("positions", "기록된 답변"),
            ("models", "응답 모델"),
            ("synthesis", "종합 기록"),
        ],
        "log" => &[
            ("at", "시각"),
            ("kind", "이벤트"),
            ("from", "보낸 참여자"),
            ("to", "받는 참여자"),
            ("seq", "Room 순번"),
        ],
        _ => return Err(Error::new("unknown desktop history view")),
    };
    if view == "capacity" {
        let observed = oms_panel_core::teams::desktop_capacity(repo, stop)?;
        if observed.is_null() {
            status = "unavailable"
        } else {
            for metric in ["teams", "invocations", "open_process_claims", "write_wip"] {
                rows.push(json!({"metric":metric,"used":observed["occupancy"][metric],"limit":observed["limits"][metric],"active":observed["active"]}));
            }
        }
    } else if let Some(id) = room.filter(|id| {
        report["room"]["id"].as_str() == Some(*id)
            && report["room"]["closed"] == false
            && report["room"]["successor"].is_null()
            && report["collection"]["ok"] == true
    }) {
        if view == "log" {
            // The TUI's stdout transitions are ephemeral. Use the canonical
            // Room sequence for desktop history and restrict it to the owner.
            if let (Some(owner), Some(messages)) = (
                report["room"]["owner"].as_str(),
                report["room"]["messages"].as_array(),
            ) {
                for m in messages.iter().rev() {
                    if !visible(m, owner) {
                        continue;
                    }
                    rows.push(json!({"at":cell(&m["ts"]),"from":cell(&m["sender"]),"to":cell(&m["recipient"]),"kind":cell(&m["message_kind"]),"seq":m["seq"].as_u64()}));
                }
            } else {
                status = "unavailable"
            }
        } else if let Some(attempts) = report["room_attempts"].as_object() {
            let mut unique = BTreeMap::new();
            for a in attempts.values() {
                if !in_room(a, id) {
                    continue;
                }
                if view == "debate" && a["tool"] != "panel-council"
                    || view == "past" && a["terminal"] != true
                {
                    continue;
                }
                if let Some(attempt) = a["attempt_id"].as_str() {
                    unique.insert(attempt, a);
                }
            }
            let mut attempts: Vec<_> = unique.into_values().collect();
            attempts.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
            for a in attempts {
                if view == "debate"
                    && let (Some(attempt), Some(thread)) =
                        (a["attempt_id"].as_str(), a["refs"]["panel_thread"].as_str())
                {
                    council_threads.insert(attempt.to_string(), thread.to_string());
                }
                rows.push(json!({"at":cell(&a["updated_at"]),"attempt":cell(&a["attempt_id"]),"provider":cell(&a["provider"]),"task":cell(&a["task_id"]),"state":cell(&a["state"])}));
            }
        } else {
            status = "unavailable"
        }
    } else {
        status = "unavailable"
    }
    let offset = page
        .checked_sub(1)
        .and_then(|p| p.checked_mul(PAGE_SIZE))
        .ok_or_else(|| Error::new("invalid page"))?;
    let next = (offset.saturating_add(PAGE_SIZE) < rows.len()).then_some(page.saturating_add(1));
    let rows: Vec<_> = rows.into_iter().skip(offset).take(PAGE_SIZE).collect();
    let mut rows = rows;
    if view == "debate" {
        for row in &mut rows {
            if let Some((count, models, synthesis)) = row["attempt"]
                .as_str()
                .and_then(|id| council_threads.get(id))
                .and_then(|thread| council_metadata(repo, thread))
            {
                row["positions"] = json!(count);
                row["models"] = if models.is_empty() {
                    Value::Null
                } else {
                    json!(models)
                };
                row["synthesis"] = json!(synthesis);
            }
        }
    }
    Ok(
        json!({"view":view,"page":page,"nextPage":next,"status":status,
         "columns":columns.iter().map(|(key,label)|json!({"key":key,"label":label})).collect::<Vec<_>>(),"rows":rows}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_excludes_other_rooms_unaddressed_messages_and_duplicate_aliases() {
        let a = json!({"attempt_id":"finished","tool":"panel-council","state":"done","terminal":true,"refs":{"panel_room_id":"room-a"}});
        let report = json!({"collection":{"ok":true},"room":{"id":"room-a","closed":false,"owner":"main","messages":[{"sender":"lead","recipient":"all","targets":["main"],"seq":1,"message_kind":"update","text":"unprojected"},{"sender":"worker","recipient":"lead","seq":2}]},
            "room_attempts":{"finished":a,"alias":a,"foreign":{"attempt_id":"bad","tool":"panel-council","terminal":true,"refs":{"panel_room_id":"room-a","panel_room":"room-b"}}}});
        let get = |room, view| {
            page(
                Path::new("."),
                &report,
                room,
                view,
                1,
                &AtomicBool::new(false),
            )
            .unwrap()
        };
        for view in ["past", "debate", "log"] {
            assert_eq!(get(None, view)["status"], "unavailable");
            assert_eq!(get(Some("room-b"), view)["status"], "unavailable");
            assert_eq!(
                get(Some("room-a"), view)["rows"].as_array().unwrap().len(),
                1
            );
        }
        assert_eq!(get(Some("room-a"), "log")["rows"][0]["seq"], 1);
        assert!(
            !get(Some("room-a"), "log")
                .to_string()
                .contains("unprojected")
        );
    }
}
