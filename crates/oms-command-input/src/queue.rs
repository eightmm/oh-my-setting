//! Editable intent followed by one explicit, reconcilable room delivery.
//!
//! Counterpart of `scripts/lib/oms_inputs/queue.py` and the state half of `common.py`.
use crate::json::{
    J, array_of, dup, get, get_mut, int_of, number, object, os_string, remove, same, set, str_of,
    string,
};
use crate::{
    HostServices, Result, UNSAFE, hash, load, room, safe_text, storage, utc_now, valid_id, validate,
};
use oms_safe_fs::Root;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const MAX_STATE: usize = 4 * 1024 * 1024;
const MAX_ITEMS: usize = 256;
const MAX_TEXT: usize = 3500;
const NOT_FOUND: &str = "input item does not exist";
const BOUND: &str = "input queue has reached its 256-item bound";

pub struct Ctx<'a> {
    pub repo: &'a Path,
    pub root: &'a Root,
    pub work: &'a Path,
    pub interrupted: &'a AtomicBool,
    pub services: &'a dyn HostServices,
}

fn untracked(c: &Ctx) -> Result<()> {
    if !crate::tracked_inputs(c.repo, c.interrupted)?.is_empty() {
        return Err("private input state must not be tracked by Git");
    }
    Ok(())
}
/// Run `body` while the host holds the exclusive queue lock; a lock timeout skips `body`.
pub fn under_lock<T>(c: &Ctx, body: impl FnOnce() -> Result<T>) -> Result<T> {
    untracked(c)?;
    let path = c.repo.join(".oms/inputs/queue.json");
    let mut body = Some(body);
    let mut out = None;
    c.services.with_queue_lock(&path, c.interrupted, &mut || {
        out = Some((body.take().ok_or(UNSAFE)?)());
        Ok(())
    })?;
    out.ok_or(UNSAFE)?
}
pub fn state(c: &Ctx) -> Result<J> {
    let Some(bytes) = crate::read_opt(c.root, Path::new(".oms/inputs/queue.json"), MAX_STATE)?
    else {
        return Ok(object([
            ("schema", number(1)),
            ("repo", os_string(c.repo.as_os_str())),
            ("revision", number(0)),
            ("items", J::Array(vec![])),
            ("feedback_batches", J::Object(vec![])),
        ]));
    };
    let value = crate::json::parse(&bytes)?;
    let ok = matches!(value, J::Object(_))
        && int_of(&value, "schema") == Some(1)
        && get(&value, "repo").is_some_and(|r| same(r, &os_string(c.repo.as_os_str())))
        && array_of(&value, "items").is_some_and(|i| i.len() <= MAX_ITEMS)
        && matches!(get(&value, "feedback_batches"), Some(J::Object(_)))
        && int_of(&value, "revision").is_some();
    if !ok {
        return Err("invalid or oversized input queue");
    }
    Ok(value)
}
pub fn write_state(c: &Ctx, value: &mut J) -> Result<()> {
    untracked(c)?;
    let revision = int_of(value, "revision").ok_or(UNSAFE)?;
    set(value, "revision", number(revision + 1));
    // The exact bytes written must stay readable under the reader's limit.
    let bytes = crate::json::encode_state(value)?;
    if bytes.len() > MAX_STATE {
        return Err("input queue exceeds its 4 MiB bound");
    }
    storage::write_queue(c.repo, &bytes, c.interrupted)
}
fn items(value: &J) -> &[J] {
    array_of(value, "items").map_or(&[], Vec::as_slice)
}
fn items_mut(value: &mut J) -> Result<&mut Vec<J>> {
    match get_mut(value, "items") {
        Some(J::Array(items)) => Ok(items),
        _ => Err(UNSAFE),
    }
}
/// The ID check is lazy, as in the legacy generator: an empty queue never validates it.
pub fn index(value: &J, key: &str) -> Result<usize> {
    let rows = items(value);
    if rows.is_empty() {
        return Err(NOT_FOUND);
    }
    valid_id(std::ffi::OsStr::new(key))?;
    rows.iter()
        .position(|row| str_of(row, "id") == Some(key))
        .ok_or(NOT_FOUND)
}
pub fn item(value: &J, key: &str) -> Result<J> {
    Ok(dup(&items(value)[index(value, key)?]))
}
fn new_id() -> Result<String> {
    Ok(format!("input-{}", crate::storage::random_hex()?))
}
fn status(row: &J) -> &str {
    str_of(row, "status").unwrap_or("")
}

pub fn attachment_refs(c: &Ctx, keys: &[String]) -> Result<Vec<J>> {
    let mut distinct = keys.to_vec();
    distinct.sort();
    distinct.dedup();
    if keys.len() > 8 || distinct.len() != keys.len() {
        return Err("an input accepts at most eight distinct attachments");
    }
    let mut result = Vec::new();
    for key in keys {
        let (record, _) = load(c.root, key)?;
        validate(c.root, &record, c.interrupted)?;
        let source = get(&record, "source").ok_or(UNSAFE)?;
        result.push(object([
            ("id", string(str_of(&record, "id").ok_or(UNSAFE)?)),
            (
                "content_sha256",
                dup(get(&record, "content_sha256").ok_or(UNSAFE)?),
            ),
            (
                "record_sha256",
                string(&hash(&crate::json::canonical(&record)?)),
            ),
            ("kind", dup(get(&record, "kind").ok_or(UNSAFE)?)),
            (
                "source_verified",
                J::Bool(get(source, "verified") == Some(&J::Bool(true))),
            ),
        ]));
    }
    Ok(result)
}
pub fn payload(row: &J) -> Result<String> {
    let text = str_of(row, "text").ok_or(UNSAFE)?;
    let mut out = format!(
        "OMS queued input / {} / {}\n\n{}",
        str_of(row, "mode").ok_or(UNSAFE)?,
        str_of(row, "id").ok_or(UNSAFE)?,
        text.trim_end_matches(['\r', '\n'])
    );
    for found in array_of(row, "attachments").ok_or(UNSAFE)? {
        let id = str_of(found, "id").ok_or(UNSAFE)?;
        out.push_str(&format!(
            "\n\nAttachment {id} ({}; {})\nsha256 {}\nRead: oms input attachment {id}",
            str_of(found, "kind").ok_or(UNSAFE)?,
            if get(found, "source_verified") == Some(&J::Bool(true)) {
                "source verified at capture"
            } else {
                "declared origin, unverified"
            },
            str_of(found, "content_sha256").ok_or(UNSAFE)?
        ));
    }
    out.push_str("\n\nDelivery records a room message; it does not prove execution or approval.");
    safe_text(&out, 4000)?;
    Ok(out)
}
pub fn make_item(
    c: &Ctx,
    route: J,
    text: &str,
    mode: &str,
    keys: &[String],
    provenance: Option<J>,
) -> Result<J> {
    if !matches!(mode, "steer" | "next") {
        return Err("mode must be steer or next");
    }
    let id = new_id()?;
    safe_text(text, MAX_TEXT)?;
    let mut row = object([
        ("created_at", string(&utc_now())),
        ("version", number(1)),
        ("status", string("queued")),
        ("worktree", os_string(c.work.as_os_str())),
        ("binding", route),
        ("mode", string(mode)),
        ("text", string(text)),
        ("attachments", J::Array(attachment_refs(c, keys)?)),
        ("message_id", string(&format!("msg-{id}"))),
        ("id", string(&id)),
    ]);
    if let Some(provenance) = provenance {
        set(&mut row, "provenance", provenance);
    }
    payload(&row)?;
    Ok(row)
}
/// Room and participant names follow the room rule, which also admits a leading `_`.
fn route_id(value: &str) -> Result<&str> {
    if room::valid_room(value) {
        Ok(value)
    } else {
        Err("expected an exact local identifier")
    }
}
pub fn binding(c: &Ctx, route_room: &str, who: &str, to: &str) -> Result<J> {
    room::binding(c.root, route_id(route_room)?, route_id(who)?, route_id(to)?)
}

pub fn add(
    c: &Ctx,
    room_id: &str,
    who: &str,
    to: &str,
    text: &str,
    mode: &str,
    keys: &[String],
) -> Result<J> {
    let route = binding(c, room_id, who, to)?;
    let row = make_item(c, route, text, mode, keys, None)?;
    under_lock(c, || {
        let mut value = state(c)?;
        if items(&value).len() >= MAX_ITEMS {
            return Err(BOUND);
        }
        items_mut(&mut value)?.push(dup(&row));
        write_state(c, &mut value)?;
        Ok(row)
    })
}
pub fn edit(
    c: &Ctx,
    key: &str,
    text: Option<&str>,
    mode: Option<&str>,
    keys: Option<&[String]>,
) -> Result<J> {
    under_lock(c, || {
        let mut value = state(c)?;
        let at = index(&value, key)?;
        let mut row = dup(&items(&value)[at]);
        if status(&row) != "queued" {
            return Err("only queued inputs may be edited");
        }
        if let Some(text) = text {
            safe_text(text, MAX_TEXT)?;
            set(&mut row, "text", string(text));
        }
        if let Some(mode) = mode {
            set(&mut row, "mode", string(mode));
        }
        if let Some(keys) = keys {
            set(&mut row, "attachments", J::Array(attachment_refs(c, keys)?));
        }
        payload(&row)?;
        let version = int_of(&row, "version").ok_or(UNSAFE)?;
        set(&mut row, "version", number(version + 1));
        set(&mut row, "edited_at", string(&utc_now()));
        items_mut(&mut value)?[at] = dup(&row);
        write_state(c, &mut value)?;
        Ok(row)
    })
}
pub fn move_item(c: &Ctx, key: &str, before: Option<&str>) -> Result<J> {
    under_lock(c, || {
        let mut value = state(c)?;
        let at = index(&value, key)?;
        let row = dup(&items(&value)[at]);
        if status(&row) != "queued" {
            return Err("only queued inputs may be reordered");
        }
        if before == Some(key) {
            return Ok(row);
        }
        if let Some(before) = before {
            let target = item(&value, before)?;
            if status(&target) != "queued" {
                return Err("move target must be queued");
            }
        }
        let rows = items_mut(&mut value)?;
        let moved = rows.remove(at);
        let to = before
            .and_then(|b| rows.iter().position(|r| str_of(r, "id") == Some(b)))
            .unwrap_or(rows.len());
        rows.insert(to, moved);
        write_state(c, &mut value)?;
        Ok(row)
    })
}
pub fn cancel(c: &Ctx, key: &str) -> Result<J> {
    under_lock(c, || {
        let mut value = state(c)?;
        let at = index(&value, key)?;
        let mut row = dup(&items(&value)[at]);
        if status(&row) != "queued" {
            return Err("only queued inputs may be cancelled; reconcile an uncertain delivery");
        }
        set(&mut row, "status", string("cancelled"));
        set(&mut row, "cancelled_at", string(&utc_now()));
        items_mut(&mut value)?[at] = dup(&row);
        write_state(c, &mut value)?;
        Ok(row)
    })
}
pub fn preview(c: &Ctx, row: &J) -> Result<J> {
    let route = get(row, "binding").ok_or(UNSAFE)?;
    let worktree = get(row, "worktree").ok_or(UNSAFE)?;
    let work = crate::repository(&crate::json::os_path(worktree)?, c.interrupted)?;
    let shared = crate::state_repository(&work, c.interrupted)?;
    if !same(&os_string(work.as_os_str()), worktree) || shared != c.repo {
        return Err("input source worktree or shared repository ownership changed");
    }
    let (room_id, from, to) = (
        str_of(route, "room").ok_or(UNSAFE)?,
        str_of(route, "from").ok_or(UNSAFE)?,
        str_of(route, "to").ok_or(UNSAFE)?,
    );
    let current = binding(c, room_id, from, to)?;
    if !same(
        get(&current, "identity").ok_or(UNSAFE)?,
        get(route, "identity").ok_or(UNSAFE)?,
    ) {
        return Err(
            "input sender, recipient, or room owner changed; create a new explicitly addressed input",
        );
    }
    let keys: Vec<String> = array_of(row, "attachments")
        .ok_or(UNSAFE)?
        .iter()
        .map(|r| str_of(r, "id").map(str::to_owned).ok_or(UNSAFE))
        .collect::<Result<_>>()?;
    let refs = J::Array(attachment_refs(c, &keys)?);
    if !same(&refs, get(row, "attachments").ok_or(UNSAFE)?) {
        return Err("immutable attachment metadata or content changed");
    }
    let text = payload(row)?;
    Ok(object([
        ("id", dup(get(row, "id").ok_or(UNSAFE)?)),
        ("mode", dup(get(row, "mode").ok_or(UNSAFE)?)),
        ("version", dup(get(row, "version").ok_or(UNSAFE)?)),
        ("message_id", dup(get(row, "message_id").ok_or(UNSAFE)?)),
        ("room", dup(get(&current, "resolved_room").ok_or(UNSAFE)?)),
        ("from", string(from)),
        ("to", string(to)),
        ("text", string(&text)),
        ("payload_sha256", string(&hash(text.as_bytes()))),
        ("bytes", number(text.len() as i64)),
        ("attachments", refs),
        (
            "at_boundary_required",
            J::Bool(str_of(row, "mode") == Some("next")),
        ),
        ("executes_model", J::Bool(false)),
    ]))
}

fn receipt(c: &Ctx, row: &J) -> Result<Option<J>> {
    let intent = get(row, "delivery")
        .filter(|v| !(matches!(v, J::Null) || matches!(v, J::Object(f) if f.is_empty())))
        .ok_or("input has no delivery intent to reconcile")?;
    let message_id = str_of(row, "message_id").ok_or(UNSAFE)?;
    let intent_text = str_of(intent, "text");
    if str_of(intent, "message_id") != Some(message_id)
        || intent_text != Some(payload(row)?.as_str())
        || str_of(intent, "payload_sha256") != intent_text.map(|t| hash(t.as_bytes())).as_deref()
    {
        return Err("delivery intent no longer matches the immutable queued payload");
    }
    let route = get(row, "binding").ok_or(UNSAFE)?;
    let history = room::history(c.root, str_of(route, "room").ok_or(UNSAFE)?)?;
    let found = room::find_delivery(
        &history,
        message_id,
        str_of(intent, "from").ok_or(UNSAFE)?,
        str_of(intent, "to").ok_or(UNSAFE)?,
        intent_text.ok_or(UNSAFE)?,
        get(route, "identity").ok_or(UNSAFE)?,
    )?;
    let Some(original) = found.original else {
        return Ok(None);
    };
    Ok(Some(object([
        ("room", string(&original.room)),
        ("seq", number(original.seq)),
        ("ts", original.ts),
        ("message_id", string(message_id)),
        (
            "payload_sha256",
            dup(get(intent, "payload_sha256").ok_or(UNSAFE)?),
        ),
        ("acknowledged", J::Bool(!found.acknowledgments.is_empty())),
        (
            "acknowledgments",
            J::Array(
                found
                    .acknowledgments
                    .iter()
                    .map(|(name, seq)| object([("room", string(name)), ("seq", number(*seq))]))
                    .collect(),
            ),
        ),
        (
            "authority",
            string("persisted room message; acknowledgment is not approval or execution proof"),
        ),
    ])))
}

/// A shared checkout is not a grant to read another task's selected input.
pub fn authorize_child_attachment(c: &Ctx, key: &str) -> Result<()> {
    const REFUSED: &str =
        "worker attachment reads require an admitted room repository and participant";
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let (Some(inherited), Some(selected), Some(who)) = (
        env("OMS_ROOM_REPO"),
        env("OMS_ROOM_ID"),
        env("OMS_ROOM_PARTICIPANT"),
    ) else {
        return Err(REFUSED);
    };
    let inherited = PathBuf::from(inherited);
    if inherited.canonicalize().unwrap_or(inherited) != c.repo {
        return Err(REFUSED);
    }
    let (selected, who) = (
        selected.to_str().ok_or(UNSAFE)?,
        who.to_str().ok_or(UNSAFE)?,
    );
    let history = room::history(c.root, selected)?;
    let projection = room::project(&history.last().ok_or(UNSAFE)?.1)?;
    let member = room::participant(&projection, who)?;
    if str_of(&member.fields, "role") == Some("main")
        || get(&member.fields, "parent").is_none_or(|p| matches!(p, J::Null) || *p == string(""))
    {
        return Err("worker attachment reads require an admitted child participant");
    }
    let key = valid_id(std::ffi::OsStr::new(key))?;
    let (record, _) = load(c.root, key)?;
    let record_sha = hash(&crate::json::canonical(&record)?);
    let value = state(c)?;
    for row in items(&value) {
        let route = get(row, "binding").ok_or(UNSAFE)?;
        if status(row) != "delivered"
            || str_of(route, "to") != Some(who)
            || !room::same_room(c.root, str_of(route, "room").ok_or(UNSAFE)?, selected)?
        {
            continue;
        }
        let Some(found) = array_of(row, "attachments")
            .ok_or(UNSAFE)?
            .iter()
            .find(|r| str_of(r, "id") == Some(key))
        else {
            continue;
        };
        if str_of(found, "record_sha256") != Some(&record_sha)
            || get(found, "content_sha256") != get(&record, "content_sha256")
        {
            continue;
        }
        let current = binding(
            c,
            str_of(route, "room").ok_or(UNSAFE)?,
            str_of(route, "from").ok_or(UNSAFE)?,
            who,
        )?;
        if same(
            get(&current, "identity").ok_or(UNSAFE)?,
            get(route, "identity").ok_or(UNSAFE)?,
        ) && receipt(c, row)?.is_some()
        {
            return Ok(());
        }
    }
    Err("attachment was not confirmed delivered to this admitted worker identity")
}

pub fn deliver(
    c: &Ctx,
    key: &str,
    apply: bool,
    at_boundary: bool,
    expected: Option<&str>,
) -> Result<J> {
    if apply {
        under_lock(c, || deliver_inner(c, key, apply, at_boundary, expected))
    } else {
        deliver_inner(c, key, apply, at_boundary, expected)
    }
}
fn deliver_inner(
    c: &Ctx,
    key: &str,
    apply: bool,
    at_boundary: bool,
    expected: Option<&str>,
) -> Result<J> {
    let mut value = state(c)?;
    let at = index(&value, key)?;
    let row = dup(&items(&value)[at]);
    if status(&row) == "delivered" {
        let mut row = row;
        set(&mut row, "deduplicated", J::Bool(true));
        return Ok(row);
    }
    if status(&row) != "queued" {
        return Err("input is not queued; reconcile uncertainty before any further delivery");
    }
    let plan = preview(c, &row)?;
    if expected.is_some_and(|digest| str_of(&plan, "payload_sha256") != Some(digest)) {
        return Err("input changed since preview; inspect its current payload digest");
    }
    if !apply {
        let mut plan = plan;
        set(&mut plan, "dry_run", J::Bool(true));
        return Ok(plan);
    }
    if str_of(&row, "mode") == Some("next") && !at_boundary {
        return Err("next-task delivery requires an explicit --at-boundary acknowledgment");
    }
    let lane = |other: &J| -> Option<(String, String, String, String)> {
        let route = get(other, "binding")?;
        Some((
            str_of(get(route, "identity")?, "root_room")?.to_owned(),
            str_of(route, "from")?.to_owned(),
            str_of(route, "to")?.to_owned(),
            str_of(other, "mode")?.to_owned(),
        ))
    };
    let mine = lane(&row).ok_or(UNSAFE)?;
    let first = items(&value)
        .iter()
        .find(|other| status(other) == "queued" && lane(other).as_ref() == Some(&mine))
        .ok_or(UNSAFE)?;
    if str_of(first, "id") != Some(key) {
        return Err("an earlier input is queued for this target and mode; reorder it explicitly");
    }
    let mut row = row;
    let mut intent = dup(&plan);
    set(&mut intent, "intent_at", string(&utc_now()));
    set(&mut row, "delivery", intent);
    set(&mut row, "status", string("uncertain"));
    items_mut(&mut value)?[at] = dup(&row);
    write_state(c, &mut value)?;
    // The uncertain intent is durable before any message can exist. A kill from
    // here on leaves `uncertain`, which `reconcile` resolves without a duplicate.
    let sent = c.services.send_room_note(
        c.repo,
        str_of(&plan, "room").ok_or(UNSAFE)?,
        str_of(&plan, "from").ok_or(UNSAFE)?,
        str_of(&plan, "to").ok_or(UNSAFE)?,
        str_of(&plan, "text").ok_or(UNSAFE)?,
        str_of(&plan, "message_id").ok_or(UNSAFE)?,
    );
    let outcome = sent
        .map_err(|_| ())
        .and_then(|()| receipt(c, &row).map_err(|_| ()));
    match outcome {
        Ok(Some(found)) => {
            set(&mut row, "status", string("delivered"));
            set(&mut row, "receipt", found);
        }
        Ok(None) => {}
        Err(()) => set(
            &mut row,
            "delivery_error",
            string("room append failed or its exact result could not be verified"),
        ),
    }
    items_mut(&mut value)?[at] = dup(&row);
    write_state(c, &mut value)?;
    Ok(row)
}
pub fn reconcile(c: &Ctx, key: &str) -> Result<J> {
    under_lock(c, || {
        let mut value = state(c)?;
        let at = index(&value, key)?;
        let mut row = dup(&items(&value)[at]);
        if !matches!(status(&row), "uncertain" | "delivered") {
            return Err("only attempted deliveries can be reconciled");
        }
        let found = receipt(c, &row)?;
        let observed = found.is_some();
        if let Some(found) = found {
            set(&mut row, "status", string("delivered"));
            set(&mut row, "receipt", found);
            remove(&mut row, "delivery_error");
        }
        set(&mut row, "last_reconciled_at", string(&utc_now()));
        items_mut(&mut value)?[at] = dup(&row);
        write_state(c, &mut value)?;
        set(&mut row, "observed", J::Bool(observed));
        set(&mut row, "retry_allowed", J::Bool(false));
        Ok(row)
    })
}
pub fn list(c: &Ctx) -> Result<J> {
    let value = state(c)?;
    Ok(object([
        ("schema", number(1)),
        ("revision", dup(get(&value, "revision").ok_or(UNSAFE)?)),
        ("items", dup(get(&value, "items").ok_or(UNSAFE)?)),
    ]))
}
