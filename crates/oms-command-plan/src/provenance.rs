use crate::engine::{parse_ts, receipt_id, room_id};
use crate::json::{self, Value, number, object, string};
use crate::storage::{self, MAX_JSON, MAX_STREAM};
use crate::{Plan, Result, error, require};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SEGMENT: usize = 256 * 1024;
pub(crate) struct History {
    pub room_id: String,
    pub room_rows: Vec<Value>,
    pub room: Value,
    pub events: Vec<Value>,
    pub attempts: Value,
}
pub(crate) fn room_projection(plan: &Plan<'_>, room: &str, raw: &[u8]) -> Result<Value> {
    let rows = storage::rows(raw)?;
    require(!rows.is_empty(), "native room history is empty")?;
    for (i, r) in rows.iter().enumerate() {
        require(
            r.get("schema").is_int(1)
                && r.text("thread") == room
                && r.get("seq").is_int((i + 1) as i128)
                && parse_ts(r.text("ts")).is_some(),
            "malformed native room history",
        )?;
        if let Some(event) = r.opt("room_event") {
            event.object()?;
        }
    }
    let result = plan.services.project_room(room, raw)?;
    require(
        result.source_sha256 == json::sha(raw)
            && result.state.text("id") == room
            && result.state.get("schema").is_int(1),
        "foreign or unbound native room projection",
    )?;
    result.state.get("participants").array()?;
    require(
        matches!(result.state.get("closed"), Value::Bool(_)),
        "native room closure is unproven",
    )?;
    Ok(result.state)
}
pub(crate) fn lifecycle_projection(plan: &Plan<'_>, raw: &[u8]) -> Result<Value> {
    let events = storage::rows(raw)?;
    let mut ids = BTreeSet::new();
    let mut seq = BTreeMap::<String, i128>::new();
    for r in &events {
        require(
            r.get("schema").is_int(1)
                && receipt_id(r.text("event_id"), 160)
                && receipt_id(r.text("attempt_id"), 160)
                && parse_ts(r.text("ts")).is_some()
                && ids.insert(r.text("event_id")),
            "malformed native lifecycle history",
        )?;
        require(
            matches!(
                r.text("event_type"),
                "attempt.created" | "attempt.state_changed" | "attempt.heartbeat" | "attempt.usage"
            ),
            "unknown lifecycle event",
        )?;
        let n = seq.entry(r.text("attempt_id").into()).or_insert(0);
        *n += 1;
        require(
            r.get("seq").is_int(*n),
            "native lifecycle sequence mismatch",
        )?;
        if let Some(refs) = r.opt("refs") {
            for (k, v) in refs.object()? {
                let key = std::str::from_utf8(k).map_err(|_| error("invalid lifecycle ref"))?;
                let value = v.str()?;
                require(
                    receipt_id(key, 160)
                        && value.chars().count() <= 240
                        && !value.contains(['\n', '\r'])
                        && !value.starts_with(['/', '\\'])
                        && !value.split('/').any(|s| s == ".."),
                    "malformed native lifecycle refs",
                )?;
            }
        }
    }
    let result = plan.services.project_lifecycle(raw)?;
    require(
        result.source_sha256 == json::sha(raw),
        "unbound lifecycle projection",
    )?;
    result.attempts.object()?;
    for id in seq.keys() {
        let projected = result.attempts.get(id);
        require(
            projected.text("attempt_id") == id
                && matches!(projected.get("terminal"), Value::Bool(_)),
            "incomplete lifecycle projection",
        )?;
    }
    Ok(result.attempts)
}
pub(crate) fn capture(plan: &Plan<'_>, room: &str) -> Result<Value> {
    require(room_id(room), "invalid completion room identity")?;
    let mut proof = object([
        ("schema", number(1)),
        ("repository_sha256", string(plan.state_git().namespace()?)),
        ("room_id", string(room)),
    ]);
    for (kind, path) in [
        ("room", format!(".oms/threads/{room}.jsonl")),
        ("lifecycle", ".oms/lifecycle/events.jsonl".into()),
    ] {
        let raw = plan.store.read(&path, MAX_STREAM)?;
        let rows = storage::rows(&raw)?;
        require(!rows.is_empty(), "captured canonical stream is empty")?;
        if kind == "room" {
            room_projection(plan, room, &raw)?;
        } else {
            lifecycle_projection(plan, &raw)?;
        }
        let mut segments = Vec::new();
        for bytes in raw.chunks(SEGMENT) {
            segments.push(
                plan.store
                    .immutable(".oms/plan/completion-segments", bytes, ".bin", SEGMENT)?
                    .0,
            );
        }
        let manifest = object([
            ("schema", number(1)),
            ("stream_sha256", string(json::sha(&raw))),
            ("segments", Value::Array(segments)),
        ]);
        let (mut reference, _) = plan.store.immutable(
            ".oms/plan/completion-prefixes",
            &json::encoded(&manifest)?,
            ".json",
            MAX_JSON,
        )?;
        reference.set("schema", number(2))?;
        reference.set("kind", string(kind))?;
        reference.set("source", string(path))?;
        reference.set("bytes", number(raw.len()))?;
        reference.set("rows", number(rows.len()))?;
        proof.set(kind, reference)?;
    }
    let (reference, _) = plan.store.immutable(
        ".oms/plan/completion-captures",
        &json::encoded(&proof)?,
        ".json",
        MAX_JSON,
    )?;
    proof.set("capture", reference)?;
    Ok(proof)
}
pub(crate) fn reference(plan: &Plan<'_>, value: &Value, limit: usize) -> Result<Vec<u8>> {
    json::keys(value, &["path", "sha256"], &[])?;
    crate::completion::relative(value.get("path").str()?)?;
    let hash = json::digest(value.get("sha256"))?;
    let raw = plan.store.read(value.text("path"), limit)?;
    require(
        json::sha(&raw) == hash,
        format!("evidence digest changed: {}", value.text("path")),
    )?;
    Ok(raw)
}
pub(crate) fn load(plan: &Plan<'_>, proof: &Value, live: bool) -> Result<History> {
    json::keys(
        proof,
        &[
            "schema",
            "repository_sha256",
            "room_id",
            "room",
            "lifecycle",
            "capture",
        ],
        &[],
    )?;
    require(
        proof.get("schema").is_int(1)
            && proof.text("repository_sha256") == plan.state_git().namespace()?
            && room_id(proof.text("room_id")),
        "unsupported or foreign captured provenance",
    )?;
    let cap = proof.get("capture");
    require(
        cap.text("path")
            == format!(
                ".oms/plan/completion-captures/{}.json",
                json::digest(cap.get("sha256"))?
            ),
        "noncanonical provenance capture",
    )?;
    let captured = json::parse(&reference(plan, cap, MAX_JSON)?)?;
    let mut without = proof.clone();
    without.remove("capture");
    require(
        captured == without,
        "prefix references differ from trusted complete capture",
    )?;
    let mut streams = Vec::new();
    for kind in ["room", "lifecycle"] {
        let item = proof.get(kind);
        json::keys(
            item,
            &[
                "schema", "kind", "source", "path", "sha256", "bytes", "rows",
            ],
            &[],
        )?;
        let schema = item.get("schema").int()?;
        let bytes = item.get("bytes").int()?;
        let count = item.get("rows").int()?;
        let source = if kind == "room" {
            format!(".oms/threads/{}.jsonl", proof.text("room_id"))
        } else {
            ".oms/lifecycle/events.jsonl".into()
        };
        require(
            [1, 2].contains(&schema)
                && item.text("kind") == kind
                && item.text("source") == source
                && (1..=MAX_STREAM as i128).contains(&bytes)
                && (1..=MAX_STREAM as i128).contains(&count)
                && item.text("path")
                    == format!(
                        ".oms/plan/completion-prefixes/{}{}",
                        json::digest(item.get("sha256"))?,
                        if schema == 1 { ".jsonl" } else { ".json" }
                    ),
            "invalid captured stream boundary or identity",
        )?;
        let ref_only = object([
            ("path", item.get("path").clone()),
            ("sha256", item.get("sha256").clone()),
        ]);
        let raw = if schema == 1 {
            reference(plan, &ref_only, MAX_STREAM)?
        } else {
            let manifest = json::parse(&reference(plan, &ref_only, MAX_JSON)?)?;
            json::keys(&manifest, &["schema", "stream_sha256", "segments"], &[])?;
            let segments = manifest.get("segments").array()?;
            require(
                manifest.get("schema").is_int(1)
                    && segments.len() == (bytes as usize).div_ceil(SEGMENT),
                "invalid captured segment manifest",
            )?;
            let mut raw = Vec::with_capacity(bytes as usize);
            for (index, s) in segments.iter().enumerate() {
                require(
                    s.text("path")
                        == format!(
                            ".oms/plan/completion-segments/{}.bin",
                            json::digest(s.get("sha256"))?
                        ),
                    "noncanonical captured segment",
                )?;
                let part = reference(plan, s, SEGMENT)?;
                require(
                    part.len() == SEGMENT.min(bytes as usize - index * SEGMENT),
                    "captured segment boundary mismatch",
                )?;
                raw.extend(part);
            }
            require(
                json::sha(&raw) == json::digest(manifest.get("stream_sha256"))?,
                "captured stream digest changed",
            )?;
            raw
        };
        require(
            raw.len() == bytes as usize && storage::rows(&raw)?.len() == count as usize,
            "captured byte/row boundary mismatch",
        )?;
        if live {
            let current = plan.store.read(&source, MAX_STREAM)?;
            require(
                current.starts_with(&raw),
                "captured canonical stream prefix changed",
            )?;
            if kind == "room" {
                room_projection(plan, proof.text("room_id"), &current)?;
            } else {
                lifecycle_projection(plan, &current)?;
            }
        }
        streams.push(raw);
    }
    let room_raw = streams.remove(0);
    let lifecycle_raw = streams.remove(0);
    let room = room_projection(plan, proof.text("room_id"), &room_raw)?;
    let attempts = lifecycle_projection(plan, &lifecycle_raw)?;
    Ok(History {
        room_id: proof.text("room_id").into(),
        room_rows: storage::rows(&room_raw)?,
        room,
        events: storage::rows(&lifecycle_raw)?,
        attempts,
    })
}
pub(crate) fn live_history(plan: &Plan<'_>, room: &str) -> Result<History> {
    let room_raw = plan
        .store
        .read(format!(".oms/threads/{room}.jsonl"), MAX_STREAM)?;
    let lifecycle_raw = plan.store.read(".oms/lifecycle/events.jsonl", MAX_STREAM)?;
    Ok(History {
        room_id: room.into(),
        room_rows: storage::rows(&room_raw)?,
        room: room_projection(plan, room, &room_raw)?,
        events: storage::rows(&lifecycle_raw)?,
        attempts: lifecycle_projection(plan, &lifecycle_raw)?,
    })
}
pub(crate) fn successors(
    plan: &Plan<'_>,
    history: &History,
    historical: bool,
) -> Result<Vec<(Vec<Value>, Value)>> {
    if historical {
        return Ok(vec![]);
    }
    let mut found = Vec::new();
    let mut state = history.room.clone();
    let mut seen = BTreeSet::from([history.room_id.clone()]);
    for _ in 0..64 {
        let next = state.text("successor");
        if next.is_empty() {
            break;
        }
        require(
            next.starts_with("room-") && json::hex(&next[5..], 12) && seen.insert(next.to_owned()),
            "invalid room successor lineage",
        )?;
        let Some(raw) = plan
            .store
            .optional(format!(".oms/threads/{next}.jsonl"), MAX_STREAM)?
        else {
            break;
        };
        let later = room_projection(plan, next, &raw)?;
        if later.text("predecessor") != state.text("id") {
            break;
        }
        found.push((storage::rows(&raw)?, later.clone()));
        state = later;
    }
    Ok(found)
}
type SwapMap = BTreeMap<String, (String, String)>;
fn swaps(logs: &[&[Value]], who: &str) -> Result<(SwapMap, String)> {
    let mut provider = None;
    let mut released = BTreeMap::new();
    for log in logs {
        for row in *log {
            let e = row.get("room_event");
            if e.text("participant") != who {
                continue;
            }
            if matches!(e.text("kind"), "join" | "main-resume") && e.text("role") == "main" {
                provider = Some(e.get("provider").str()?.to_owned());
            } else if e.text("kind") == "main-swap" {
                let old = provider
                    .take()
                    .ok_or_else(|| error("unproven main swap lineage"))?;
                let next = e.get("provider").str()?.to_owned();
                require(
                    released
                        .insert(e.get("attempt").str()?.into(), (old, next.clone()))
                        .is_none(),
                    "duplicate main swap attempt",
                )?;
                provider = Some(next);
            }
        }
    }
    Ok((
        released,
        provider.ok_or_else(|| error("original native main membership unproven"))?,
    ))
}
pub(crate) fn participant(attempt: &Value) -> Result<&str> {
    let value = attempt
        .get("refs")
        .opt("panel_room_participant")
        .unwrap_or(attempt.get("attempt_id"))
        .str()?;
    require(
        receipt_id(value, 160) && value.as_bytes()[0].is_ascii_alphanumeric(),
        "malformed explicit native participant identity",
    )?;
    Ok(value)
}
fn released(attempt: &Value, swaps: &BTreeMap<String, (String, String)>) -> bool {
    swaps
        .get(attempt.text("attempt_id"))
        .is_some_and(|(from, _)| {
            attempt.text("provider") == from
                && attempt.text("state") == "abandoned"
                && attempt.text("reason_code") == "main_swapped"
                && attempt.get("refs").text("panel_owner") == attempt.text("provider")
        })
}
pub(crate) fn bindings(
    events: &[Value],
    who: &str,
    provider: &str,
    room: &str,
    swaps: &BTreeMap<String, (String, String)>,
) -> Result<Vec<Value>> {
    let mut related = BTreeSet::new();
    for e in events {
        e.get("refs").object().or_else(|_| {
            if e.opt("refs").is_none() {
                Ok(&[][..])
            } else {
                Err(error("malformed native history refs"))
            }
        })?;
        if e.text("attempt_id") == who || e.get("refs").text("panel_room_participant") == who {
            related.insert(e.text("attempt_id").to_owned());
        }
    }
    let mut out = Vec::new();
    for ident in related {
        let own = events
            .iter()
            .filter(|e| e.text("attempt_id") == ident)
            .collect::<Vec<_>>();
        require(
            own.first()
                .is_some_and(|e| e.text("event_type") == "attempt.created"),
            "native participant creation unproven",
        )?;
        let first = own[0];
        let mut current = object([
            ("attempt_id", string(&ident)),
            ("provider", first.get("provider").clone()),
            ("tool", first.get("tool").clone()),
            ("refs", crate::json::empty()),
            ("state", string("queued")),
            ("reason_code", string("")),
        ]);
        for e in own {
            for name in ["provider", "tool"] {
                require(
                    e.opt(name).is_none_or(|v| v == current.get(name)),
                    "contradictory native participant history",
                )?;
            }
            let mut refs = current.get("refs").clone();
            if let Some(value) = e.opt("refs") {
                for (k, v) in value.object()? {
                    refs.set(
                        std::str::from_utf8(k).map_err(|_| error("invalid ref"))?,
                        v.clone(),
                    )?;
                }
            }
            current.set("refs", refs)?;
            require(
                participant(&current)? == who
                    && current.text("tool") == "panel-main"
                    && current.get("refs").text("panel_role") == "main"
                    && current.get("refs").text("panel_room_id") == room
                    && current
                        .get("refs")
                        .opt("panel_owner")
                        .is_none_or(|v| v.str() == Ok(current.text("provider"))),
                "contradictory native participant history",
            )?;
            if e.text("event_type") == "attempt.state_changed" {
                current.set("state", e.get("to_state").clone())?;
                current.set(
                    "reason_code",
                    e.opt("reason_code").cloned().unwrap_or(string("")),
                )?;
            }
        }
        let terminal = matches!(
            current.text("state"),
            "done" | "failed" | "cancelled" | "timed_out" | "abandoned"
        );
        current.set("terminal", Value::Bool(terminal))?;
        require(
            current.text("provider") == provider || released(&current, swaps),
            "contradictory native participant provider history",
        )?;
        out.push(current);
    }
    Ok(out)
}
fn membership(
    plan: &Plan<'_>,
    history: &History,
    who: &str,
    provider: &str,
    window: Option<(&str, &str)>,
    historical: bool,
) -> Result<()> {
    let later = successors(plan, history, historical)?;
    let mut logs = vec![history.room_rows.as_slice()];
    logs.extend(later.iter().map(|(rows, _)| rows.as_slice()));
    let (swap, final_provider) = swaps(&logs, who)?;
    let joins = history
        .room_rows
        .iter()
        .filter(|r| {
            r.get("room_event").text("participant") == who
                && matches!(r.get("room_event").text("kind"), "join" | "main-resume")
        })
        .collect::<Vec<_>>();
    require(
        !joins.is_empty()
            && joins[0].get("room_event").text("role") == "main"
            && (joins[0].get("room_event").text("provider") == provider
                || swap.values().any(|(_, to)| to == provider)),
        "original native room membership is unproven",
    )?;
    let closing = |r: &Value| {
        r.text("role") == "closed"
            || r.get("room_event").text("kind") == "closed"
            || r.get("room_event").text("kind") == "main-resume"
                && r.get("room_event").text("replaces") == who
            || r.get("room_event").text("participant") == who
                && r.get("room_event").text("kind") == "leave"
    };
    if let Some((started, ended)) = window {
        require(
            joins[0].text("ts") <= started,
            "READ parent was not enrolled at evidence point",
        )?;
        require(
            !history
                .room_rows
                .iter()
                .any(|r| closing(r) && r.text("ts") <= ended)
                && !joins.iter().skip(1).any(|r| r.text("ts") <= ended),
            "historical READ crosses recycled or ambiguous membership",
        )?;
        for (rows, _) in later {
            let start = rows
                .iter()
                .position(|r| r.get("room_event").text("kind") == "carried")
                .map_or(rows.len(), |i| i + 1);
            require(
                !rows[start..].iter().any(|r| {
                    r.text("ts") <= ended
                        && (closing(r)
                            || r.get("room_event").text("participant") == who
                                && matches!(
                                    r.get("room_event").text("kind"),
                                    "join" | "main-resume"
                                ))
                }),
                "historical READ crosses successor membership boundary",
            )?;
        }
        return Ok(());
    }
    require(
        joins.len() == 1 && !history.room_rows.iter().any(closing),
        "actual parent room context is unproven or recycled",
    )?;
    for state in std::iter::once(&history.room).chain(later.iter().map(|(_, s)| s)) {
        let member = state
            .get("participants")
            .array()?
            .iter()
            .find(|m| m.text("participant") == who)
            .ok_or_else(|| error("native room main missing"))?;
        require(
            state.get("closed") == &Value::Bool(false)
                && member.get("joined") == &Value::Bool(true)
                && member.text("role") == "main",
            "actual parent room context is unproven or recycled",
        )?;
    }
    for (rows, state) in &later {
        let joins = rows
            .iter()
            .filter(|r| {
                r.get("room_event").text("participant") == who
                    && matches!(r.get("room_event").text("kind"), "join" | "main-resume")
            })
            .count();
        require(
            joins == 1 && !rows.iter().any(closing) && state.get("closed") == &Value::Bool(false),
            "successor native membership is unproven or recycled",
        )?;
    }
    require(
        final_provider == provider,
        "actual parent room provider differs from declared owner",
    )
}
pub(crate) fn swapped_root(plan: &Plan<'_>, room: &str, who: &str, provider: &str) -> Result<()> {
    let history = live_history(plan, room)?;
    require(
        history.room.get("successor") == &Value::Null,
        "swapped Main Room was superseded",
    )?;
    let (swap, _) = swaps(&[&history.room_rows], who)?;
    let prior = history
        .room_rows
        .iter()
        .rev()
        .map(|row| row.get("room_event"))
        .find(|event| event.text("kind") == "main-swap" && event.text("participant") == who)
        .ok_or_else(|| error("swapped Main lacks canonical Room lineage"))?;
    let related = bindings(&history.events, who, provider, room, &swap)?;
    require(
        prior.text("provider") == provider
            && related.iter().any(|attempt| {
                attempt.text("attempt_id") == prior.text("attempt") && released(attempt, &swap)
            }),
        "swapped Main lacks verified predecessor release",
    )?;
    membership(plan, &history, who, provider, None, false)
}
pub(crate) fn owner(
    plan: &Plan<'_>,
    bundle: &Value,
    task: &Value,
    history: &History,
    historical: bool,
    attempt: &str,
) -> Result<Value> {
    let who = bundle.get("owner").text("participant");
    let provider = bundle.get("owner").text("parent_provider");
    require(
        who == task.text("claimed_by_participant"),
        "declared claim owner mismatch",
    )?;
    if !historical {
        require(
            who == plan.context.participant
                && history.room_id == plan.context.room_id
                && plan.context.room_id.starts_with("room-")
                && json::hex(&plan.context.room_id[5..], 12),
            "actual parent caller differs from current declared claim owner",
        )?;
    }
    let later = successors(plan, history, historical)?;
    let mut logs = vec![history.room_rows.as_slice()];
    logs.extend(later.iter().map(|(r, _)| r.as_slice()));
    let (swap, _) = swaps(&logs, who)?;
    let related = bindings(&history.events, who, provider, &history.room_id, &swap)?;
    let active = related
        .iter()
        .filter(|a| a.get("terminal") == &Value::Bool(false))
        .collect::<Vec<_>>();
    require(
        active.len() == 1
            && active[0].text("attempt_id") == attempt
            && matches!(
                active[0].text("state"),
                "starting"
                    | "working"
                    | "waiting_input"
                    | "waiting_approval"
                    | "verifying"
                    | "review"
            ),
        "current native panel-main owner binding unproven or ambiguous",
    )?;
    let projected = history.attempts.get(attempt);
    require(
        projected.get("terminal") == &Value::Bool(false)
            && projected.text("state") == active[0].text("state"),
        "native projected owner binding differs",
    )?;
    membership(plan, history, who, provider, None, historical)?;
    Ok(object([
        ("participant", string(who)),
        ("parent_provider", string(provider)),
        ("room_id", string(&history.room_id)),
    ]))
}
pub(crate) fn read_parent(
    plan: &Plan<'_>,
    history: &History,
    parent: &Value,
    child: &[Value],
    who: &str,
    provider: &str,
    historical: bool,
) -> Result<Value> {
    let first = child
        .first()
        .ok_or_else(|| error("READ creation boundary unproven"))?;
    require(
        first.text("event_type") == "attempt.created",
        "READ creation boundary unproven",
    )?;
    let boundary = history
        .events
        .iter()
        .position(|e| e.get("event_id") == first.get("event_id"))
        .ok_or_else(|| error("READ boundary not in lifecycle"))?;
    let own = history.events[..boundary]
        .iter()
        .filter(|e| e.get("attempt_id") == parent.get("attempt_id"))
        .collect::<Vec<_>>();
    let room = own
        .first()
        .map(|e| e.get("refs").text("panel_room_id"))
        .unwrap_or("");
    require(
        room.starts_with("room-") && json::hex(&room[5..], 12),
        "exact historical native room unproven",
    )?;
    // A resumed owner may now be in a carried room while the READ's parent
    // joined its sealed predecessor. Preserve that original room identity;
    // lifecycle ancestry continues to use the captured event prefix.
    let other;
    let membership_history = if room == history.room_id {
        history
    } else {
        let raw = plan
            .store
            .read(format!(".oms/threads/{room}.jsonl"), MAX_STREAM)?;
        other = History {
            room_id: room.into(),
            room_rows: storage::rows(&raw)?,
            room: room_projection(plan, room, &raw)?,
            events: vec![],
            attempts: crate::json::empty(),
        };
        &other
    };
    let later = successors(plan, membership_history, historical)?;
    let mut logs = vec![membership_history.room_rows.as_slice()];
    logs.extend(later.iter().map(|(r, _)| r.as_slice()));
    let (swap, _) = swaps(&logs, who)?;
    let provider = if parent.text("provider") != provider && released(parent, &swap) {
        parent.text("provider")
    } else {
        provider
    };
    let related = bindings(&history.events[..boundary], who, provider, room, &swap)?;
    let active = related
        .iter()
        .filter(|a| a.get("terminal") == &Value::Bool(false))
        .collect::<Vec<_>>();
    require(
        active.len() == 1
            && active[0].get("attempt_id") == parent.get("attempt_id")
            && matches!(
                active[0].text("state"),
                "starting"
                    | "working"
                    | "waiting_input"
                    | "waiting_approval"
                    | "verifying"
                    | "review"
            ),
        "READ parent was not unique active native owner at creation",
    )?;
    let terminal = child
        .iter()
        .filter(|e| e.text("event_type") == "attempt.state_changed" && e.text("to_state") == "done")
        .collect::<Vec<_>>();
    require(
        terminal.len() == 1,
        "READ successful terminal boundary unproven",
    )?;
    membership(
        plan,
        membership_history,
        who,
        provider,
        Some((first.text("ts"), terminal[0].text("ts"))),
        historical,
    )?;
    Ok(active[0].clone())
}
