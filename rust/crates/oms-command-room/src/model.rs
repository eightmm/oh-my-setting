use crate::value::{Value, array, obj, strings};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use unicode_general_category::{GeneralCategory as G, get_general_category};

pub const ROLES: &[&str] = &["main", "worker", "advisor", "reviewer", "researcher"];
pub const KINDS: &[&str] = &[
    "question", "answer", "note", "decision", "handoff", "status",
];
pub const MAIN_OWNERS: &[&str] = &["claude", "codex", "pi", "antigravity"];
pub const MAX_PARTICIPANTS: usize = 32;
pub const MAX_CALLS: usize = 480;
pub const MAX_MESSAGES: usize = 1024;
pub const MAX_METADATA_BYTES: usize = 8192;
pub const MAIN_RESERVE: usize = 8;
pub const CALL_RESERVE: usize = 64;
pub const MESSAGE_RESERVE: usize = 128;
pub const MAX_ROOMS: usize = 64;
pub const MAX_SCAN_ENTRIES: usize = 1024;
pub const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;
pub const ROLLOVER_MESSAGES: usize = MAX_MESSAGES - MESSAGE_RESERVE;
pub const ROLLOVER_CALLS: usize = MAX_CALLS - CALL_RESERVE;
pub const CARRY_MESSAGES: usize = ROLLOVER_MESSAGES / 2;
pub const CARRY_CALLS: usize = ROLLOVER_CALLS / 2;
pub const MAX_HOPS: usize = 64;
pub const TAIL_BYTES: usize = 8192;
pub const DELIVERY: &str = "next safe point or explicit poll; acknowledgment is not approval";
pub const ACK_AFTER: u64 = 300;
pub const REPEAT_AFTER: u64 = 300;
pub const ASKER_AFTER: u64 = 600;
pub const OVERDUE_AFTER: u64 = 900;
pub const PERSON_AFTER: u64 = 1200;
pub const SECOND_REMINDER_AFTER: u64 = 1800;
pub const MAX_REMINDERS: usize = 3;

pub fn identifier<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || value.starts_with(['-', '.'])
    {
        return Err(Error::new(format!("invalid {label}")));
    }
    Ok(value)
}
pub fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn safe_room_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['/', '\\'])
        && !value.contains(['\\', ':'])
        && value.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
pub fn model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:-".contains(&b))
}
pub fn main_owner_matches(harness: &str, owner: &str) -> bool {
    match harness {
        "pi" => matches!(owner, "codex" | "pi"),
        "agy" => owner == "antigravity",
        _ => harness == owner,
    }
}
pub fn clean(value: &str, limit: usize) -> String {
    let text: String = value
        .chars()
        .filter(|c| !matches!(c, '\u{fe0e}' | '\u{fe0f}' | '\u{200d}'))
        .map(|c| match get_general_category(c) {
            G::SpaceSeparator | G::LineSeparator | G::ParagraphSeparator => ' ',
            G::Control if matches!(c, '\t' | '\n' | '\r') => ' ',
            G::Control | G::Format | G::PrivateUse | G::Surrogate | G::Unassigned => '?',
            _ => c,
        })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= limit {
        text
    } else {
        format!(
            "{}...",
            text.chars()
                .take(limit.saturating_sub(3))
                .collect::<String>()
        )
    }
}
pub fn declared_scopes(role: &str, scopes: &[String]) -> Result<()> {
    if scopes.len() > 16 {
        return Err(Error::new("at most sixteen declared scopes"));
    }
    if role == "researcher" && !scopes.is_empty() {
        return Err(Error::new("researchers own no write scope"));
    }
    if matches!(role, "advisor" | "reviewer") && !scopes.is_empty() {
        return Err(Error::new("advisors and reviewers own no write scope"));
    }
    for s in scopes {
        if oms_contract_markdown::scope::normalize(s).map_err(|e| Error::new(e.to_string()))? != *s
        {
            return Err(Error::new("use normalized repository-relative scopes"));
        }
    }
    Ok(())
}
pub fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a == "."
        || b == "."
        || a.starts_with(&format!("{b}/"))
        || b.starts_with(&format!("{a}/"))
}
pub fn required(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "created" | "carried" => &[],
        "join" => &["participant", "provider", "role", "label"],
        "bind" => &["participant", "consumer"],
        "main-swap" => &["participant", "provider", "model", "attempt", "harness"],
        "scope" | "leave" | "closed" | "received" => &["participant"],
        "describe" => &["participant", "label"],
        "message" => &["id", "sender", "recipient", "message_kind"],
        "publication" => &["id", "digest", "status"],
        "successor" => &["participant", "target"],
        "call-result" => &[
            "event_id",
            "task_id",
            "call_id",
            "attempt_id",
            "role",
            "access",
            "verify_status",
        ],
        "review-dispatch" | "review-dispatch-failed" => &[
            "event_id",
            "task_id",
            "call_id",
            "patch_sha256",
            "review_call_id",
        ],
        _ => return None,
    })
}
pub fn allowed(kind: &str) -> &'static [&'static str] {
    match kind {
        "created" => &["kind", "task_id", "participant", "predecessor"],
        "join" => &[
            "kind",
            "participant",
            "provider",
            "model",
            "role",
            "label",
            "consumer",
            "owns",
            "parent",
            "dispatch_role",
            "purpose",
            "state",
            "alias",
            "task_id",
        ],
        "main-swap" => &[
            "kind",
            "participant",
            "provider",
            "model",
            "attempt",
            "harness",
        ],
        "bind" => &["kind", "participant", "consumer", "previous"],
        "scope" => &["kind", "participant", "owns"],
        "describe" => &["kind", "participant", "label"],
        "leave" | "closed" => &["kind", "participant"],
        "message" => &[
            "kind",
            "id",
            "sender",
            "recipient",
            "message_kind",
            "reply_to",
            "quiet",
            "audience",
        ],
        "received" => &["kind", "participant", "messages"],
        "publication" => &[
            "kind",
            "id",
            "digest",
            "status",
            "delivery_unknown",
            "turn_id",
            "retry",
            "target",
        ],
        "successor" => &["kind", "participant", "target"],
        "carried" => &["kind"],
        "call-result" => &[
            "kind",
            "event_id",
            "task_id",
            "call_id",
            "attempt_id",
            "role",
            "access",
            "exit",
            "verify_status",
            "verify_exit",
            "artifact",
            "patch",
            "patch_sha256",
            "changed_files",
            "needs_review",
        ],
        "review-dispatch" | "review-dispatch-failed" => &[
            "kind",
            "event_id",
            "task_id",
            "call_id",
            "patch_sha256",
            "review_call_id",
            "attempt",
        ],
        _ => &[],
    }
}
pub fn event_shape(event: &Value) -> Result<()> {
    let fields = event
        .as_object()
        .ok_or_else(|| Error::new("invalid persisted room event"))?;
    if fields.is_empty() {
        return Ok(());
    }
    let kind = event.text("kind");
    let required = required(kind).ok_or_else(|| Error::new("invalid persisted room event"))?;
    if required.iter().any(|k| !event[k].is_string()) {
        return Err(Error::new("invalid persisted room event"));
    }
    for k in [
        "model",
        "consumer",
        "previous",
        "parent",
        "reply_to",
        "task_id",
        "turn_id",
        "target",
        "participant",
        "harness",
        "predecessor",
    ] {
        if !event[k].is_null() && !event[k].is_string() {
            return Err(Error::new("invalid room event field"));
        }
    }
    for k in ["owns", "messages", "audience"] {
        if event.get(k).is_some() {
            strings(&event[k])?;
        }
    }
    if kind == "scope" && event.get("owns").is_none() {
        return Err(Error::new("missing declared scopes"));
    }
    if kind == "received" && event.get("messages").is_none() {
        return Err(Error::new("missing room receipt ids"));
    }
    if kind == "join" {
        if !event["alias"].is_null() {
            let a = event.text("alias");
            if a.is_empty()
                || a.len() > 8
                || !a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(Error::new("invalid dispatched alias"));
            }
        }
        if !event["dispatch_role"].is_null() {
            let r = event.text("dispatch_role");
            if r.is_empty()
                || r.len() > 64
                || !r.as_bytes()[0].is_ascii_lowercase()
                || !r
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(Error::new("invalid dispatched role"));
            }
        }
        if !event["purpose"].is_null() && !event["purpose"].is_string() {
            return Err(Error::new("invalid join purpose"));
        }
        if !event["state"].is_null() && event.text("state") != "queued" {
            return Err(Error::new("invalid initial join state"));
        }
    }
    if kind == "publication" && event["delivery_unknown"].as_bool().is_none() {
        return Err(Error::new("invalid room delivery receipt"));
    }
    if kind == "message" && event.get("quiet").is_some() && event["quiet"].as_bool().is_none() {
        return Err(Error::new("invalid quiet message marker"));
    }
    if kind == "publication" && event.get("target").is_some() && !event["target"].is_string() {
        return Err(Error::new("invalid room delivery target"));
    }
    if kind == "call-result" {
        if !ROLES.contains(&event.text("role"))
            || !matches!(event.text("access"), "read" | "write")
            || event["exit"].as_i64().is_none()
            || event["needs_review"].as_bool().is_none()
            || !matches!(event.text("verify_status"), "passed" | "failed" | "not_run")
            || !event["verify_exit"].is_null() && event["verify_exit"].as_i64().is_none()
        {
            return Err(Error::new("invalid structured call result"));
        }
        if let Some(v) = event.get("changed_files") {
            strings(v)?;
        }
    }
    if matches!(kind, "review-dispatch" | "review-dispatch-failed") {
        if !hex(event.text("patch_sha256"), 64) {
            return Err(Error::new("invalid review patch digest"));
        }
        if event["attempt"].as_u64().is_none_or(|v| v < 1) {
            return Err(Error::new("invalid automatic review attempt"));
        }
    }
    if kind == "describe"
        && (fields
            .iter()
            .any(|(k, _)| !allowed(kind).iter().any(|a| k == a.as_bytes()))
            || event.text("label").is_empty()
            || event.text("label") != clean(event.text("label"), 120))
    {
        return Err(Error::new("use a bounded plain participant label"));
    }
    Ok(())
}
pub fn member<'a>(state: &'a Value, who: &str) -> Result<&'a Value> {
    identifier(who, "participant")?;
    state
        .list("participants")
        .iter()
        .find(|p| p.text("participant") == who && p.flag("joined"))
        .ok_or_else(|| Error::new("participant has not joined this room"))
}
pub fn project(rows: &[Value]) -> Result<Value> {
    for r in rows {
        event_shape(r.get("room_event").unwrap_or(&obj([])))?;
        if r.get("text").is_some_and(|v| !v.is_string()) {
            return Err(Error::new("invalid room text"));
        }
    }
    let created = rows
        .iter()
        .find(|r| r["room_event"].text("kind") == "created")
        .ok_or_else(|| Error::new("this thread is not a work room"))?;
    let mut members: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut receipts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut owner = Value::Null;
    let mut publication = Value::Null;
    let mut publications = obj([]);
    let mut successor = Value::Null;
    let mut swaps = Vec::new();
    let mut carried = false;
    for r in rows {
        let e = &r["room_event"];
        let who = e.text("participant");
        match e.text("kind") {
            "join" => {
                let at = members.iter().position(|p| p.text("participant") == who);
                let prev = at.map(|i| &members[i]);
                let first = prev
                    .map(|p| p["initial_consumer"].clone())
                    .unwrap_or_else(|| e["consumer"].clone());
                let initial = prev
                    .map(|p| p["initial_seq"].clone())
                    .unwrap_or_else(|| r["seq"].clone());
                let mut p = e.clone();
                p.set("joined", true);
                p.set("seq", r["seq"].clone());
                p.set(
                    "joined_at",
                    r.get("ts").cloned().unwrap_or_else(|| "".into()),
                );
                p.set("initial_consumer", first);
                p.set("initial_seq", initial);
                if owner.is_null() && e.text("role") == "main" {
                    owner = who.into();
                }
                if let Some(at) = at {
                    members[at] = p;
                } else {
                    members.push(p);
                }
            }
            "main-swap" => {
                let p = members
                    .iter_mut()
                    .find(|p| p.text("participant") == who)
                    .ok_or_else(|| Error::new("main swap has no participant"))?;
                swaps.push(obj([
                    ("participant", who.into()),
                    ("from", p["provider"].clone()),
                    ("from_model", p["model"].clone()),
                    ("to", e["provider"].clone()),
                    ("harness", e["harness"].clone()),
                    ("model", e["model"].clone()),
                    ("attempt", e["attempt"].clone()),
                    ("ts", r["ts"].clone()),
                ]));
                p.set("provider", e["provider"].clone());
                p.set("model", e["model"].clone());
                p.set("consumer", Value::Null);
                p.set("initial_consumer", Value::Null);
            }
            "leave" => {
                if let Some(p) = members.iter_mut().find(|p| p.text("participant") == who) {
                    p.set("joined", false);
                }
            }
            "describe" | "bind" | "scope" => {
                let p = members
                    .iter_mut()
                    .find(|p| p.text("participant") == who)
                    .ok_or_else(|| Error::new("room metadata has no participant"))?;
                match e.text("kind") {
                    "describe" => {
                        if !p.flag("joined") || p.text("role") != "main" {
                            return Err(Error::new("describe requires an enrolled main"));
                        }
                        p.set("label", e["label"].clone());
                        p.set("label_override", e["label"].clone());
                    }
                    "bind" => {
                        p.set("consumer", e["consumer"].clone());
                        if p["initial_consumer"].is_null() && p["seq"] == p["initial_seq"] {
                            p.set("initial_consumer", e["consumer"].clone());
                        }
                    }
                    _ => p.set("owns", e["owns"].clone()),
                }
            }
            "message" => {
                let targets = if e.flag("quiet") || e.text("message_kind") == "status" {
                    array([])
                } else if e.get("audience").is_some() {
                    e["audience"].clone()
                } else if e.text("recipient") == "all" {
                    array(
                        members
                            .iter()
                            .filter(|p| {
                                p.flag("joined") && p.text("participant") != e.text("sender")
                            })
                            .map(|p| p.text("participant")),
                    )
                } else {
                    array([e.text("recipient")])
                };
                let mut m = e.clone();
                m.set("targets", targets);
                m.set("text", r.get("text").cloned().unwrap_or_else(|| "".into()));
                m.set("seq", r["seq"].clone());
                m.set("ts", r["ts"].clone());
                messages.push(m);
            }
            "received" => {
                for mid in strings(&e["messages"])? {
                    receipts.entry(mid).or_default().insert(who.to_owned());
                }
            }
            "publication" => {
                publication = e.clone();
                publication.set("seq", r["seq"].clone());
                publications.set(
                    e.get("target").and_then(Value::as_str).unwrap_or("codex"),
                    publication.clone(),
                );
            }
            "successor" => successor = e["target"].clone(),
            "carried" => carried = true,
            _ => (),
        }
    }
    let answered: BTreeSet<String> = messages
        .iter()
        .filter(|m| m.text("message_kind") == "answer")
        .map(|m| m.text("reply_to").to_owned())
        .collect();
    for m in &mut messages {
        let done = receipts.get(m.text("id")).cloned().unwrap_or_default();
        let pending: Vec<Value> = m
            .list("targets")
            .iter()
            .filter(|p| !done.contains(p.as_str().unwrap_or("")))
            .cloned()
            .collect();
        let is_answered = answered.contains(m.text("id"));
        m.set("received_by", array(done.iter().map(String::as_str)));
        m.set("pending_for", pending);
        m.set("answered", is_answered);
    }
    let e = &created["room_event"];
    Ok(obj([
        ("schema", 1u64.into()),
        ("kind", "oms-room".into()),
        ("id", created["thread"].clone()),
        (
            "title",
            created
                .get("text")
                .cloned()
                .unwrap_or_else(|| "Shared work".into()),
        ),
        ("owner", owner),
        ("task_id", e["task_id"].clone()),
        ("creator", e["participant"].clone()),
        (
            "closed",
            rows.iter()
                .any(|r| r.text("role") == "closed" || r["room_event"].text("kind") == "closed")
                .into(),
        ),
        ("participants", members.into()),
        ("messages", messages.into()),
        (
            "swaps",
            swaps
                .into_iter()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .into(),
        ),
        ("publication", publication),
        ("publications", publications),
        ("successor", successor),
        ("predecessor", e["predecessor"].clone()),
        ("carried", carried.into()),
        ("delivery", DELIVERY.into()),
    ]))
}
pub fn visible(row: &Value, who: &str, joined_seq: u64) -> bool {
    let e = &row["room_event"];
    e.text("kind") == "message"
        && !e.flag("quiet")
        && e.text("message_kind") != "status"
        && row.number("seq") > joined_seq
        && e.text("sender") != who
        && (e.text("recipient") == "all" || e.text("recipient") == who)
}
pub fn visible_to(row: &Value, who: &str, joined_seq: u64) -> bool {
    visible(row, who, joined_seq)
        && row["room_event"].get("audience").is_none_or(|v| {
            v.as_array()
                .is_some_and(|v| v.iter().any(|p| p.as_str() == Some(who)))
        })
}
pub fn unread_messages(state: &Value, who: &str) -> Result<Vec<Value>> {
    let p = member(state, who)?;
    Ok(state
        .list("messages")
        .iter()
        .filter(|m| {
            m.list("pending_for")
                .iter()
                .any(|v| v.as_str() == Some(who))
                && visible_to(
                    &obj([("seq", m["seq"].clone()), ("room_event", (*m).clone())]),
                    who,
                    p.number("seq"),
                )
        })
        .cloned()
        .collect())
}
pub fn age(stamp: &str) -> u64 {
    let Some(raw) = stamp.get(..19) else {
        return 0;
    };
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|t| crate::unix_seconds().saturating_sub(t.and_utc().timestamp().max(0) as u64))
        .unwrap_or(0)
}
pub fn open_questions(state: &Value, recipient: Option<&str>, asker: Option<&str>) -> Vec<Value> {
    let live: BTreeSet<_> = state
        .list("participants")
        .iter()
        .filter(|p| p.flag("joined"))
        .map(|p| p.text("participant"))
        .collect();
    state
        .list("messages")
        .iter()
        .filter(|m| {
            m.text("message_kind") == "question"
                && !m.flag("answered")
                && m.list("targets")
                    .iter()
                    .any(|p| live.contains(p.as_str().unwrap_or("")))
                && recipient.is_none_or(|r| m.list("targets").iter().any(|p| p.as_str() == Some(r)))
                && asker.is_none_or(|a| m.text("sender") == a)
        })
        .map(|m| {
            obj([
                ("id", m["id"].clone()),
                ("sender", m["sender"].clone()),
                ("recipient", m["recipient"].clone()),
                ("ts", m["ts"].clone()),
                ("age", age(m.text("ts")).into()),
            ])
        })
        .collect()
}
pub fn reminder_id(question: &str, tier: u64) -> String {
    let legacy = format!("remind-{question}-{tier}");
    if legacy.len() <= 160 {
        legacy
    } else {
        format!("reminder-{}-{tier}", crate::digest(question.as_bytes()))
    }
}
pub fn review_ids(call: &str, digest: &str, attempt: u64) -> (String, String, String) {
    let stem = format!("{call}-{digest}-{attempt}");
    (
        format!("review-dispatch-{stem}"),
        format!("review-dispatch-failed-{stem}"),
        format!("call-{}", &crate::digest(stem.as_bytes())[..16]),
    )
}
pub fn controller_review_ids(e: &Value) -> bool {
    let (claim, failed, reviewer) = review_ids(
        e.text("call_id"),
        e.text("patch_sha256"),
        e.number("attempt"),
    );
    e.text("review_call_id") == reviewer
        && e.text("event_id")
            == if e.text("kind") == "review-dispatch" {
                claim
            } else {
                failed
            }
}
pub fn structured_of(rows: &[Value]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for row in rows {
        let e = &row["room_event"];
        if matches!(
            e.text("kind"),
            "call-result" | "review-dispatch" | "review-dispatch-failed"
        ) {
            event_shape(e)?;
            if e.text("kind") == "call-result" || controller_review_ids(e) {
                out.push(row.clone());
            }
        }
    }
    Ok(out)
}
pub fn prompt_message(message: &Value, state: &Value, who: &str) -> bool {
    let member = state
        .list("participants")
        .iter()
        .find(|p| p.text("participant") == who);
    let sender = state
        .list("participants")
        .iter()
        .find(|p| p.text("participant") == message.text("sender"));
    if member.is_none_or(|p| p.text("role") != "main")
        || sender.is_none_or(|p| p.text("role") != "worker")
        || message.text("message_kind") != "note"
        || !message["reply_to"].is_null()
    {
        return true;
    }
    use std::sync::LazyLock;
    static ACTION: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(r"(?i)[?？]|\b(ask|question|need|help|block\w*|fail\w*|error\w*|unable|cannot|can['’]t|",r"verdict|gate|decision|approv\w*|reject\w*|complet\w*|pass\w*|success\w*|succeed\w*|done|finished|ready|",r"denied|missing|unavailable|conflict|stuck|stopped|timeout|timed out)\b|질문|차단|실패|완료|결정|승인")).expect("literal regex")
    });
    static ROUTINE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(running|working|progress|started|starting|implement\w*|inspect\w*|check\w*|test\w*|investigat\w*|prepar\w*|scop\w*|read\w*|updat\w*)\b").expect("literal regex")
    });
    ACTION.is_match(message.text("text")) || !ROUTINE.is_match(message.text("text"))
}
