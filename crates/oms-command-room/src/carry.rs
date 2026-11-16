use crate::core::Carry;
use crate::model::*;
use crate::storage::Store;
use crate::value::{Value, array, obj, strings};
use crate::{Error, Result, Room, digest};
use std::collections::{BTreeMap, BTreeSet};

pub fn successor_id(old: &str) -> String {
    format!(
        "room-{}",
        &digest(format!("successor:{old}").as_bytes())[..12]
    )
}
#[derive(Clone, Debug)]
pub struct CarryWrite {
    pub event: Value,
    pub text: String,
    pub timestamp: Option<String>,
}
#[derive(Clone, Debug)]
pub struct CarryPlan {
    pub writes: Vec<CarryWrite>,
    pub dropped: Vec<Value>,
}
fn push(w: &mut Vec<CarryWrite>, event: Value, text: impl Into<String>, timestamp: Option<String>) {
    w.push(CarryWrite {
        event,
        text: text.into(),
        timestamp,
    });
}
fn ids(values: &[Value]) -> BTreeSet<String> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn rooted(who: &str, joined: &BTreeMap<String, Value>) -> bool {
    let mut parent = joined[who].text("parent");
    let mut seen = BTreeSet::from([who]);
    while !parent.is_empty() {
        let Some(p) = joined.get(parent) else {
            return false;
        };
        if !seen.insert(parent) {
            return false;
        }
        parent = p.text("parent");
    }
    true
}
/// Compute before sealing. Re-running against the successor is an idempotent
/// reconciliation, preserving identities, native anchors, scopes and mail times.
pub fn carry_plan(state: &Value, current: Option<&Value>, bounded: bool) -> Result<CarryPlan> {
    let joined: BTreeMap<String, Value> = state
        .list("participants")
        .iter()
        .filter(|p| p.flag("joined"))
        .map(|p| (p.text("participant").to_owned(), p.clone()))
        .collect();
    let members: BTreeMap<String, Value> = joined
        .iter()
        .filter(|(who, _)| rooted(who, &joined))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let results: BTreeMap<String, Value> = state
        .list("messages")
        .iter()
        .filter(|m| m.text("id").starts_with("result-"))
        .map(|m| (m.text("id").to_owned(), m.clone()))
        .collect();
    let empty = obj([]);
    let current = current.unwrap_or(&empty);
    let there: BTreeMap<String, Value> = current
        .list("participants")
        .iter()
        .map(|p| (p.text("participant").to_owned(), p.clone()))
        .collect();
    let present: BTreeSet<String> = there
        .iter()
        .filter(|(_, p)| p.flag("joined"))
        .map(|(k, _)| k.clone())
        .collect();
    let mut keep: BTreeSet<String> = members
        .iter()
        .filter(|(who, p)| {
            p.text("parent").is_empty()
                || results
                    .get(&format!("result-{who}"))
                    .is_none_or(|m| !m.list("pending_for").is_empty())
                || present.contains(*who)
        })
        .map(|(k, _)| k.clone())
        .collect();
    let finished: BTreeSet<String> = members
        .keys()
        .filter(|who| results.contains_key(&format!("result-{who}")))
        .cloned()
        .collect();
    let questions = open_questions(state, None, None);
    let indexed: BTreeMap<String, Value> = state
        .list("messages")
        .iter()
        .map(|m| (m.text("id").to_owned(), m.clone()))
        .collect();
    let known: BTreeMap<String, Value> = current
        .list("messages")
        .iter()
        .map(|m| (m.text("id").to_owned(), m.clone()))
        .collect();
    let (chosen, essential, dropped) = loop {
        let mut chosen: BTreeMap<String, Value> = BTreeMap::new();
        let mut essential = BTreeSet::new();
        for m in state.list("messages") {
            if ids(m.list("pending_for"))
                .iter()
                .any(|p| keep.contains(p) && !finished.contains(p))
            {
                chosen.insert(m.text("id").to_owned(), m.clone());
                let recipient = m.text("recipient");
                if keep.contains(recipient)
                    && !finished.contains(recipient)
                    && members
                        .get(recipient)
                        .is_some_and(|p| !p.text("parent").is_empty())
                {
                    essential.insert(m.text("id").to_owned());
                }
            }
        }
        for who in &keep {
            let mid = format!("result-{who}");
            if results.contains_key(&mid) {
                essential.insert(mid);
            }
        }
        let mut asked = BTreeSet::new();
        for q in &questions {
            let m = &indexed[q.text("id")];
            let sender = m.text("sender");
            if !ids(m.list("targets")).iter().any(|p| keep.contains(p))
                || members
                    .get(sender)
                    .is_some_and(|p| !p.text("parent").is_empty())
                    && asked.contains(sender)
            {
                continue;
            }
            asked.insert(sender.to_owned());
            essential.insert(q.text("id").to_owned());
        }
        let mut latest: BTreeMap<String, String> = BTreeMap::new();
        for m in state.list("messages") {
            if m.text("message_kind") == "status"
                && keep.contains(m.text("sender"))
                && members[m.text("sender")].text("role") == "main"
            {
                latest.insert(m.text("sender").to_owned(), m.text("id").to_owned());
            }
        }
        essential.extend(latest.into_values());
        for mid in &essential {
            if let Some(m) = indexed.get(mid) {
                chosen.insert(mid.clone(), m.clone());
            }
        }
        chosen.retain(|_, m| {
            members.contains_key(m.text("sender"))
                && (m.text("recipient") == "all" || members.contains_key(m.text("recipient")))
        });
        essential.retain(|mid| chosen.contains_key(mid));
        let mut optional = chosen
            .iter()
            .filter(|(mid, _)| !essential.contains(*mid))
            .map(|(_, m)| m.clone())
            .collect::<Vec<_>>();
        optional.sort_by_key(|m| std::cmp::Reverse(m.number("seq")));
        let dropped = optional
            .into_iter()
            .skip(MESSAGE_RESERVE.saturating_sub(essential.len()))
            .collect::<Vec<_>>();
        for m in &dropped {
            chosen.remove(m.text("id"));
        }
        let mut answered: BTreeSet<String> = chosen
            .values()
            .chain(known.values())
            .filter(|m| m.text("message_kind") == "answer")
            .map(|m| m.text("reply_to").to_owned())
            .collect();
        for m in state.list("messages") {
            if m.text("message_kind") == "answer"
                && !answered.contains(m.text("reply_to"))
                && members.contains_key(m.text("sender"))
                && members.contains_key(m.text("recipient"))
                && (chosen.contains_key(m.text("reply_to"))
                    || known.contains_key(m.text("reply_to")))
            {
                chosen.insert(m.text("id").to_owned(), m.clone());
                answered.insert(m.text("reply_to").to_owned());
            }
        }
        let mut grown = keep.clone();
        for m in chosen.values() {
            grown.insert(m.text("sender").to_owned());
            if m.text("recipient") != "all" {
                grown.insert(m.text("recipient").to_owned());
            }
        }
        for who in grown.clone() {
            let mut parent = members[&who].text("parent");
            while !parent.is_empty() && !grown.contains(parent) {
                grown.insert(parent.to_owned());
                parent = members[parent].text("parent");
            }
        }
        if grown == keep {
            break (chosen, essential, dropped);
        }
        keep = grown;
    };
    if bounded
        && (essential.len() > CARRY_MESSAGES
            || keep
                .iter()
                .filter(|w| !members[*w].text("parent").is_empty())
                .count()
                > CARRY_CALLS
            || keep
                .iter()
                .filter(|w| members[*w].text("parent").is_empty())
                .count()
                > MAX_PARTICIPANTS)
    {
        return Err(Error::new("rollover carry exceeds reserve"));
    }
    let mut queue = keep.iter().cloned().collect::<Vec<_>>();
    queue.sort_by_key(|w| members[w].number("seq"));
    let mut order = Vec::new();
    let mut placed = BTreeSet::new();
    fn place(
        who: &str,
        members: &BTreeMap<String, Value>,
        placed: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if placed.contains(who) {
            return;
        }
        let parent = members[who].text("parent");
        if !parent.is_empty() {
            place(parent, members, placed, order);
        }
        placed.insert(who.to_owned());
        order.push(who.to_owned());
    }
    for who in queue {
        place(&who, &members, &mut placed, &mut order);
    }
    let mut writes = Vec::new();
    let mut gone = present
        .iter()
        .filter(|w| !joined.contains_key(*w))
        .cloned()
        .collect::<Vec<_>>();
    gone.sort_by_key(|w| there[w].number("seq"));
    for who in &gone {
        push(
            &mut writes,
            obj([
                ("kind", "leave".into()),
                ("participant", who.clone().into()),
            ]),
            "Participant left",
            None,
        );
    }
    let mut held: BTreeSet<String> = keep
        .iter()
        .map(|w| members[w].text("consumer").to_owned())
        .collect();
    held.extend(
        there
            .iter()
            .filter(|(w, p)| p.flag("joined") && !gone.contains(*w))
            .map(|(_, p)| p.text("consumer").to_owned()),
    );
    let mut holding: BTreeMap<String, String> = there
        .iter()
        .filter(|(w, p)| p.flag("joined") && !gone.contains(*w))
        .map(|(w, p)| (w.clone(), p.text("consumer").to_owned()))
        .collect();
    let mut queue = Vec::new();
    for who in order {
        let p = &members[&who];
        let bindable = p.text("role") == "main" && MAIN_OWNERS.contains(&p.text("provider"));
        let mut now = if present.contains(&who) {
            there.get(&who).cloned()
        } else {
            None
        };
        if now.as_ref().is_some_and(|n| {
            n["consumer"] != p["consumer"] && (!bindable || p.text("consumer").is_empty())
        }) {
            push(
                &mut writes,
                obj([
                    ("kind", "leave".into()),
                    ("participant", who.clone().into()),
                ]),
                "Participant left",
                None,
            );
            holding.remove(&who);
            now = None;
        }
        queue.push((who, now, bindable));
    }
    while !queue.is_empty() {
        let ready = queue
            .iter()
            .position(|(who, now, _)| {
                let p = &members[who];
                !holding
                    .iter()
                    .any(|(w, c)| !c.is_empty() && w != who && c == p.text("consumer"))
                    && (now.is_some()
                        || p.text("parent").is_empty()
                        || holding.contains_key(p.text("parent")))
            })
            .unwrap_or(0);
        let (who, now, bindable) = queue.remove(ready);
        let p = &members[&who];
        holding.insert(who.clone(), p.text("consumer").to_owned());
        let mut now = if let Some(now) = now {
            now
        } else {
            let mut e = obj([
                ("kind", "join".into()),
                ("participant", who.clone().into()),
                ("provider", p["provider"].clone()),
                ("role", p["role"].clone()),
                ("model", p["model"].clone()),
                ("label", p["label"].clone()),
                ("owns", p.get("owns").cloned().unwrap_or_else(|| array([]))),
            ]);
            // Dispatch identity and original alias are essential, not presentation data.
            for k in [
                "parent",
                "task_id",
                "dispatch_role",
                "purpose",
                "state",
                "alias",
            ] {
                if let Some(v) = p.get(k)
                    && !v.is_null()
                {
                    e.set(k, v.clone());
                }
            }
            let first = p.text("initial_consumer");
            let consumer = if bindable
                && !p.text("consumer").is_empty()
                && !first.is_empty()
                && (!held.contains(first) || first == p.text("consumer"))
            {
                first
            } else {
                p.text("consumer")
            };
            if !consumer.is_empty() {
                e.set("consumer", consumer);
            }
            push(
                &mut writes,
                e.clone(),
                format!("Joined: {}", p.text("label")),
                None,
            );
            e
        };
        // Main swaps occurring between the first carry and seal must reach the
        // successor before it accepts writes. Their original sealed proofs are
        // replayed separately by Room::carry, never fabricated by this planner.
        if !p.text("label_override").is_empty() && now["label_override"] != p["label_override"] {
            push(
                &mut writes,
                obj([
                    ("kind", "describe".into()),
                    ("participant", who.clone().into()),
                    ("label", p["label_override"].clone()),
                ]),
                "Work description updated",
                None,
            );
        }
        if now.get("owns").cloned().unwrap_or_else(|| array([]))
            != p.get("owns").cloned().unwrap_or_else(|| array([]))
        {
            push(
                &mut writes,
                obj([
                    ("kind", "scope".into()),
                    ("participant", who.clone().into()),
                    ("owns", p.get("owns").cloned().unwrap_or_else(|| array([]))),
                ]),
                format!(
                    "Scope: {}",
                    strings(&p["owns"]).unwrap_or_default().join(", ")
                ),
                None,
            );
        }
        if !p.text("consumer").is_empty() && now["consumer"] != p["consumer"] {
            let mut e = obj([
                ("kind", "bind".into()),
                ("participant", who.into()),
                ("consumer", p["consumer"].clone()),
            ]);
            if !now.text("consumer").is_empty() {
                e.set("previous", now["consumer"].clone());
            }
            push(&mut writes, e, "Native session rebound", None);
        }
        now.remove("kind");
    }
    let mut chosen = chosen.into_values().collect::<Vec<_>>();
    chosen.sort_by_key(|m| m.number("seq"));
    let chosen_ids: BTreeSet<_> = chosen.iter().map(|m| m.text("id").to_owned()).collect();
    for m in chosen {
        if known.contains_key(m.text("id")) {
            continue;
        }
        let mut e = obj([
            ("kind", "message".into()),
            ("id", m["id"].clone()),
            ("sender", m["sender"].clone()),
            ("recipient", m["recipient"].clone()),
            ("message_kind", m["message_kind"].clone()),
        ]);
        if m.flag("quiet") {
            e.set("quiet", true);
        }
        if !m.text("reply_to").is_empty()
            && (chosen_ids.contains(m.text("reply_to")) || known.contains_key(m.text("reply_to")))
        {
            e.set("reply_to", m["reply_to"].clone());
        }
        if m.text("recipient") == "all" && m.text("message_kind") != "status" {
            e.set(
                "audience",
                m.list("targets")
                    .iter()
                    .filter(|p| keep.contains(p.as_str().unwrap_or("")))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        push(
            &mut writes,
            e,
            m.text("text"),
            m["ts"].as_str().map(str::to_owned),
        );
    }
    let mut receipts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in state.list("messages") {
        if !chosen_ids.contains(m.text("id")) && !known.contains_key(m.text("id")) {
            continue;
        }
        let known = known.get(m.text("id"));
        let done = known
            .map(|m| ids(m.list("received_by")))
            .unwrap_or_default();
        let targets = known
            .map(|m| ids(m.list("targets")))
            .unwrap_or_else(|| ids(m.list("received_by")));
        for who in strings(&m["received_by"])? {
            if keep.contains(&who) && !done.contains(&who) && targets.contains(&who) {
                receipts
                    .entry(who)
                    .or_default()
                    .push(m.text("id").to_owned());
            }
        }
    }
    for (who, mids) in receipts {
        for chunk in mids.chunks(200) {
            push(
                &mut writes,
                obj([
                    ("kind", "received".into()),
                    ("participant", who.clone().into()),
                    ("messages", chunk.to_vec().into()),
                ]),
                "Messages consumed before rollover",
                None,
            );
        }
    }
    Ok(CarryPlan {
        writes,
        dropped: dropped
            .into_iter()
            .filter(|m| !known.contains_key(m.text("id")))
            .collect(),
    })
}
impl Room<'_> {
    pub fn carry(&self, state: &Value, new: &str, bounded: bool) -> Result<Vec<Value>> {
        if self.session.child {
            return Err(Error::new("a worker cannot carry a room"));
        }
        let source_id = state.text("id");
        if new != successor_id(source_id) || self.predecessor(new)?.as_deref() != Some(source_id) {
            return Err(Error::new("carry does not name an exact derived successor"));
        }
        // A supplied projection selects the source, never supplies authority.
        // One fresh validated source snapshot is shared by the entire plan.
        let source = self.snapshot(source_id)?;
        let fresh = project(&source.rows)?;
        let state = &fresh;
        let mut current = project(&self.snapshot(new)?.rows)?;
        // Reconcile provider/model changes only from actual predecessor swap rows.
        // This cannot manufacture a release proof or speak for a native session.
        for p in current.list("participants").to_vec() {
            if let Some(wanted) = state
                .list("participants")
                .iter()
                .find(|w| w["participant"] == p["participant"] && w.flag("joined"))
                && (p["provider"] != wanted["provider"] || p["model"] != wanted["model"])
            {
                let source = self.snapshot(state.text("id"))?;
                let swaps = source
                    .rows
                    .iter()
                    .filter(|r| {
                        r["room_event"].text("kind") == "main-swap"
                            && r["room_event"]["participant"] == p["participant"]
                    })
                    .collect::<Vec<_>>();
                let mut matched = false;
                for row in swaps {
                    let e = &row["room_event"];
                    if e["provider"] == wanted["provider"] && e["model"] == wanted["model"] {
                        self.append_at(new, e, row.text("text"), Carry::Replay(None))?;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return Err(Error::new(
                        "carried main identity has no predecessor swap proof",
                    ));
                }
            }
        }
        current = project(&self.snapshot(new)?.rows)?;
        let plan = carry_plan(state, Some(&current), bounded)?;
        let writes = plan
            .writes
            .iter()
            .map(|write| {
                (
                    &write.event,
                    write.text.as_str(),
                    Carry::Replay(write.timestamp.as_deref()),
                )
            })
            .collect::<Vec<_>>();
        self.append_batch_at(new, &writes)?;
        Ok(plan.dropped)
    }
    fn carried(&self, id: &str) -> Result<bool> {
        Ok(project(&self.snapshot(id)?.rows)?.flag("carried"))
    }
    fn complete(&self, old: &str, new: &str) -> Result<()> {
        if self.carried(new)? {
            return Ok(());
        }
        let snapshot = self.snapshot(old)?;
        let seal = snapshot
            .rows
            .last()
            .map(|r| r["room_event"].clone())
            .unwrap_or_else(|| obj([]));
        if seal.text("kind") != "successor" || seal.text("target") != new {
            return Err(Error::new(format!("room {old} is not sealed into {new}")));
        }
        let state = project(&snapshot.rows)?;
        let dropped = self.carry(&state, new, false)?;
        let note = format!("rollover-unread-{}", &digest(old.as_bytes())[..12]);
        if !dropped.is_empty()
            && !project(&self.snapshot(new)?.rows)?
                .list("messages")
                .iter()
                .any(|m| m.text("id") == note)
        {
            self.append_at(new,&obj([("kind","message".into()),("id",note.into()),("sender",seal["participant"].clone()),("recipient","all".into()),("message_kind","note".into())]),&format!("Older unread mail stays in {old}; read it with oms room show --id {old} --exact"),Carry::Replay(None))?;
        }
        self.append_at(
            new,
            &obj([("kind", "carried".into())]),
            &format!("Carried from {old}"),
            Carry::Replay(None),
        )?;
        Ok(())
    }
    fn finish_carry(&self, id: &str) -> Result<()> {
        let Some(old) = self.predecessor(id)? else {
            return Ok(());
        };
        if self.carried(id)? || self.tail_successor(&old)?.as_deref() != Some(id) {
            return Ok(());
        }
        let guard = self.storage()?.rollover_lock(&old, self.stop)?;
        guard.check()?;
        if !self.session.child {
            self.complete(&old, id)?;
        }
        Ok(())
    }
    pub fn settle(&self, id: &str) -> Result<String> {
        let target = self.resolve(id)?;
        self.finish_carry(&target)?;
        Ok(target)
    }
    pub fn rollover(&self, old: &str, actor: Option<&str>) -> Result<String> {
        if self.session.child {
            return Err(Error::new("a worker cannot roll a room over"));
        }
        identifier(old, "room")?;
        self.finish_carry(old)?;
        let store = self.storage()?;
        let guard = store.rollover_lock(old, self.stop)?;
        let current = self.resolve(old)?;
        let new = successor_id(old);
        if current != old {
            if self.tail_successor(old)?.as_deref() == Some(new.as_str())
                && self.predecessor(&new)?.as_deref() == Some(old)
            {
                guard.check()?;
                self.complete(old, &new)?;
            }
            return Ok(current);
        }
        let state = project(&self.snapshot(old)?.rows)?;
        if state.flag("closed") {
            return Err(Error::new("room is closed"));
        }
        let mut mains = state
            .list("participants")
            .iter()
            .filter(|p| p.flag("joined") && p.text("role") == "main" && p.text("parent").is_empty())
            .collect::<Vec<_>>();
        mains.sort_by_key(|p| p.number("seq"));
        let chosen = mains
            .iter()
            .find(|p| actor == Some(p.text("participant")))
            .copied()
            .or_else(|| mains.first().copied())
            .ok_or_else(|| Error::new("rollover needs an enrolled main"))?;
        carry_plan(&state, None, true)?;
        guard.check()?;
        // Recovery only removes an empty successor under both legacy locks.
        let append_guard = store.lock(&new, self.stop)?;
        if store.threads.exists(&Store::name(&new))? {
            let snapshot = self.snapshot(&new)?;
            if snapshot.bytes.is_empty() {
                append_guard
                    .check()
                    .map_err(|e| Error::new(e.to_string()))?;
                store
                    .threads
                    .unlink(&Store::name(&new), Some(&snapshot.stat))?;
                self.invalidate(&new);
            }
        }
        append_guard
            .release()
            .map_err(|e| Error::new(e.to_string()))?;
        if !store.threads.exists(&Store::name(&new))? {
            self.create(
                Some(&new),
                state.text("title"),
                state["task_id"].as_str(),
                state["creator"].as_str(),
                Some(old),
            )?;
        } else {
            let snapshot = self.snapshot(&new)?;
            if snapshot.rows.len() == 1
                && snapshot.rows[0].flag("live")
                && snapshot.rows[0]["room_event"].is_null()
            {
                let e = Self::created_event(
                    state["task_id"].as_str(),
                    state["creator"].as_str(),
                    Some(old),
                )?;
                self.append_at(&new, &e, state.text("title"), Carry::No)?;
            } else if self.predecessor(&new)?.as_deref() != Some(old) {
                return Err(Error::new("successor room id collision"));
            }
        }
        if !self.carried(&new)? {
            self.carry(&state, &new, true)?;
        }
        guard.check()?;
        self.append_at(
            old,
            &obj([
                ("kind", "successor".into()),
                ("participant", chosen["participant"].clone()),
                ("target", new.clone().into()),
            ]),
            &format!("Room continues as {new}"),
            Carry::No,
        )?;
        guard.check()?;
        self.complete(old, &new)?;
        Ok(new)
    }
}
