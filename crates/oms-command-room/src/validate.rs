use crate::core::Carry;
use crate::model::*;
use crate::value::{Value, encode, strings};
use crate::{Attempt, Error, OperationsRead, Result, Room};
use std::collections::{BTreeMap, BTreeSet};

fn id(e: &Value, k: &str, label: &str) -> Result<()> {
    identifier(
        e[k].as_str()
            .ok_or_else(|| Error::new(format!("invalid {label}")))?,
        label,
    )?;
    Ok(())
}
fn same_review(a: &Value, b: &Value) -> bool {
    a["call_id"] == b["call_id"] && a["patch_sha256"] == b["patch_sha256"]
}
impl Room<'_> {
    pub(crate) fn checked_leave_parent<'s>(
        &'s self,
        state: &Value,
        id: &str,
        parent: &str,
    ) -> Result<Box<dyn OperationsRead + 's>> {
        let read = self.operations()?;
        let owner = read
            .attempts()
            .iter()
            .find(|a| a.attempt_id == parent)
            .ok_or_else(|| Error::new("checked leave native parent unavailable"))?;
        if owner.tool != "panel-main"
            || owner.terminal
            || !["working", "blocked"].contains(&owner.state.as_str())
            || !owner.refs.text("panel_room_id").eq(id)
            || !state.list("participants").iter().any(|p| {
                p.text("participant") == parent
                    && p.text("role") == "main"
                    && p.flag("joined")
                    && p.text("parent").is_empty()
            })
            || read
                .attempts()
                .iter()
                .filter(|a| {
                    a.tool == "panel-main" && !a.terminal && a.refs.text("panel_room_id") == id
                })
                .count()
                != 1
        {
            return Err(Error::new("checked leave native parent changed; reconcile"));
        }
        read.check()?;
        Ok(read)
    }
    /// Read-only preflight, never an append permit. `append` repeats validation
    /// under its canonical lock and retains producer guards through publication.
    pub fn validate_event(&self, rows: &[Value], event: &Value, text: &str) -> Result<bool> {
        self.validate_event_locked(rows, event, text, Carry::No, &mut Vec::new(), false)
    }
    /// Pure/producer checks run under the same canonical append lock as publication.
    /// Carry is a private typed context, never an ambient environment capability.
    pub(crate) fn validate_event_locked<'s>(
        &'s self,
        rows: &[Value],
        e: &Value,
        text: &str,
        carry: Carry<'_>,
        proofs: &mut Vec<Box<dyn OperationsRead + 's>>,
        text_checked: bool,
    ) -> Result<bool> {
        let kind = e.text("kind");
        event_shape(e)?;
        if required(kind).is_none() {
            return Err(Error::new("invalid room event"));
        }
        if e.as_object().is_none_or(|v| {
            v.iter()
                .any(|(k, _)| !allowed(kind).iter().any(|a| k == a.as_bytes()))
        }) || kind == "message" && e.get("audience").is_some() && !carry.active()
        {
            return Err(Error::new("unknown room event fields"));
        }
        if !e.utf8_strings() {
            return Err(Error::new("room metadata cannot be encoded as UTF-8"));
        }
        let metadata = encode(e, false, false)?;
        if metadata.len() > MAX_METADATA_BYTES {
            return Err(Error::new("room metadata is oversized"));
        }
        if !crate::outbound_clean(&metadata)? {
            return Err(Error::new(
                "room metadata contains sensitive-looking content or could not be checked",
            ));
        }
        if !text_checked && !crate::outbound_clean(text.as_bytes())? {
            return Err(Error::new(
                "turn looks sensitive; it would be replayed into other agents' prompts",
            ));
        }
        let room_id = rows.first().map(|r| r.text("thread")).unwrap_or("");
        if self.session.child
            && (carry.active()
                || rows.is_empty()
                || !self.bound(room_id)?
                || !matches!(kind, "message" | "received")
                || e.get("sender")
                    .or_else(|| e.get("participant"))
                    .and_then(Value::as_str)
                    != self.session.participant.as_deref())
        {
            return Err(Error::new(
                "a worker can only message or acknowledge as its admitted room participant",
            ));
        }
        if kind == "created" {
            if rows.len() != 1
                || !rows[0].flag("live")
                || rows
                    .iter()
                    .any(|r| r["room_event"].as_object().is_some_and(|v| !v.is_empty()))
            {
                return Err(Error::new("room already exists"));
            }
            if !e.text("participant").is_empty() {
                id(e, "participant", "creator")?;
            }
            if !e.text("task_id").is_empty() {
                id(e, "task_id", "task")?;
            }
            if !e["predecessor"].is_null() {
                id(e, "predecessor", "predecessor room")?;
                if e.text("predecessor") == room_id {
                    return Err(Error::new("a room cannot succeed itself"));
                }
            }
            return Ok(true);
        }
        let state = project(rows)?;
        if !state["successor"].is_null() {
            if kind == "successor" && e["target"] == state["successor"] {
                return Ok(false);
            }
            return Err(Error::new(format!(
                "room continues as {}",
                state.text("successor")
            )));
        }
        if !state["predecessor"].is_null() {
            if carry.active() && state.flag("carried") {
                if kind == "carried" {
                    return Ok(false);
                }
                return Err(Error::new("room carry is complete"));
            }
            if !carry.active() && !state.flag("carried") {
                return Err(Error::new("room carry is unfinished"));
            }
        } else if carry.active() || kind == "carried" {
            return Err(Error::new("only a successor room takes carried state"));
        }
        if kind == "carried" {
            if self.tail_successor(state.text("predecessor"))?.as_deref() != Some(room_id) {
                return Err(Error::new(
                    "a carry completes only after its predecessor is sealed",
                ));
            }
            return Ok(true);
        }
        if self.session.child {
            let p = member(&state, self.session.participant.as_deref().unwrap_or(""))?;
            if p.text("role") == "main" {
                return Err(Error::new("a worker cannot speak as a native main"));
            }
            if kind == "message" {
                if e.text("id").starts_with("result-") {
                    return Err(Error::new(
                        "result handoffs are written by the panel, not by a worker",
                    ));
                }
                if e.text("id").starts_with("judgment-") || e.flag("quiet") {
                    return Err(Error::new(
                        "judgment notices and quiet mail are written by the panel, not by a worker",
                    ));
                }
                if p.text("parent").is_empty() || e.text("recipient") != p.text("parent") {
                    return Err(Error::new(format!(
                        "a worker can only message its parent {}",
                        if p.text("parent").is_empty() {
                            "<missing>"
                        } else {
                            p.text("parent")
                        }
                    )));
                }
            }
        }
        if kind == "closed" {
            id(e, "participant", "participant")?;
            if let Some(caller) = &self.session.participant
                && (caller != e.text("participant") || !self.bound(room_id)?)
            {
                return Err(Error::new(
                    "close must name this session's participant and room",
                ));
            }
            if e["participant"] != state["creator"] {
                member(&state, e.text("participant"))?;
            }
            return Ok(!state.flag("closed"));
        }
        if state.flag("closed") && !matches!(kind, "received" | "leave") {
            return Err(Error::new("room is closed"));
        }
        let structured = if matches!(
            kind,
            "call-result" | "review-dispatch" | "review-dispatch-failed"
        ) {
            self.structured_rows(room_id, rows)?
                .into_iter()
                .map(|r| r["room_event"].clone())
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        if let Some(old) = structured
            .iter()
            .find(|old| old.text("kind") == kind && old["event_id"] == e["event_id"])
        {
            if kind == "review-dispatch" {
                return Err(Error::new("review patch digest already claimed"));
            }
            if old == e {
                return Ok(false);
            }
            return Err(Error::new(
                "structured room event id conflicts with existing content",
            ));
        }
        match kind {
            "successor" => {
                let p = member(&state, e.text("participant"))?;
                if p.text("role") != "main" || !p.text("parent").is_empty() {
                    return Err(Error::new("a rollover is recorded by an enrolled main"));
                }
                if e.text("target") != crate::carry::successor_id(room_id) {
                    return Err(Error::new(
                        "successor room id does not derive from this room",
                    ));
                }
                if self.predecessor(e.text("target"))?.as_deref() != Some(room_id) {
                    return Err(Error::new(
                        "successor room does not name this room as its predecessor",
                    ));
                }
            }
            "main-swap" => {
                if !e
                    .text("attempt")
                    .strip_prefix("att_")
                    .is_some_and(|v| hex(v, 32))
                {
                    return Err(Error::new("invalid swapped main attempt"));
                }
                if let Some(old) = rows
                    .iter()
                    .map(|r| &r["room_event"])
                    .find(|v| v.text("kind") == "main-swap" && v["attempt"] == e["attempt"])
                {
                    if old == e {
                        return Ok(false);
                    }
                    return Err(Error::new("swapped main release is unverified"));
                }
                if carry.active() {
                    let old = state.text("predecessor");
                    let source = self.snapshot(old)?;
                    let proof = source.rows.iter().any(|r| &r["room_event"] == e);
                    let before = project(&source.rows)?;
                    let source_member = member(&before, e.text("participant"))?;
                    if !proof
                        || source_member["provider"] != e["provider"]
                        || source_member["model"] != e["model"]
                    {
                        return Err(Error::new("carried main swap has no original proof"));
                    }
                    return Ok(true);
                }
                // A carried swap's proof stays in its mutually sealed predecessor.
                for old in self.predecessors(room_id, true)? {
                    if let Some(consumed) = self
                        .snapshot(&old)?
                        .rows
                        .iter()
                        .map(|r| &r["room_event"])
                        .find(|v| v.text("kind") == "main-swap" && v["attempt"] == e["attempt"])
                    {
                        if consumed == e {
                            return Ok(false);
                        }
                        return Err(Error::new("swapped main release is unverified"));
                    }
                }
                let read = self.operations()?;
                let mut related = Vec::new();
                for attempt in read.attempts() {
                    if attempt.tool == "panel-main"
                        && attempt
                            .refs
                            .get("panel_room_participant")
                            .and_then(Value::as_str)
                            .unwrap_or(&attempt.attempt_id)
                            == e.text("participant")
                        && self.same_room(attempt.refs.text("panel_room_id"), room_id)?
                    {
                        related.push(attempt);
                    }
                }
                let old = related
                    .last()
                    .ok_or_else(|| Error::new("swapped main release is unverified"))?;
                if old.attempt_id != e.text("attempt") {
                    return Err(Error::new("swapped main release is unverified"));
                }
                self.validate_main_swap(&state, e, old, true)?;
                read.check()?;
                proofs.push(read);
            }
            "describe" => {
                let p = member(&state, e.text("participant"))?;
                if p.text("role") != "main" {
                    return Err(Error::new("describe requires an enrolled main"));
                }
                if !carry.active()
                    && (self.session.room.is_some() || self.session.participant.is_some())
                    && (!self.bound(room_id)?
                        || self.session.participant.as_deref() != Some(e.text("participant")))
                {
                    return Err(Error::new(
                        "describe must name this session's main and room",
                    ));
                }
                if p["label_override"] == e["label"] {
                    return Ok(false);
                }
            }
            "bind" => {
                let p = member(&state, e.text("participant"))?;
                let consumer = e.text("consumer");
                if p.text("role") != "main" || !MAIN_OWNERS.contains(&p.text("provider")) {
                    return Err(Error::new(
                        "native binding is for an enrolled supported main",
                    ));
                }
                if !hex(consumer, 32) {
                    return Err(Error::new("invalid native session binding"));
                }
                if !p.text("consumer").is_empty() {
                    if p.text("consumer") == consumer {
                        return Ok(false);
                    }
                    if e["previous"] != p["consumer"] {
                        return Err(Error::new(
                            "native identity is already bound; leave and rejoin before changing it",
                        ));
                    }
                }
                if state
                    .list("participants")
                    .iter()
                    .any(|p| p.flag("joined") && p.text("consumer") == consumer)
                {
                    return Err(Error::new(
                        "native session already has a participant in this room",
                    ));
                }
            }
            "scope" => {
                if let Some(caller) = &self.session.participant
                    && !carry.active()
                    && caller != e.text("participant")
                {
                    return Err(Error::new("only a participant can declare its own scope"));
                }
                let p = member(&state, e.text("participant"))?;
                let scopes = strings(&e["owns"])?;
                declared_scopes(p.text("role"), &scopes)?;
                if !carry.active() && !p.text("parent").is_empty() && !scopes.is_empty() {
                    self.refuse_finished_call(room_id, rows, e.text("participant"))?;
                }
                if p.get("owns")
                    .cloned()
                    .unwrap_or_else(|| crate::value::array([]))
                    == e["owns"]
                {
                    return Ok(false);
                }
            }
            "publication" => {
                id(e, "id", "publication")?;
                id(e, "status", "delivery status")?;
                if !hex(e.text("digest"), 64) {
                    return Err(Error::new("invalid publication digest"));
                }
                if e["delivery_unknown"].as_bool().is_none()
                    || e.get("retry").is_some_and(|v| v.as_bool().is_none())
                {
                    return Err(Error::new("invalid publication receipt"));
                }
                if !e.text("turn_id").is_empty() {
                    id(e, "turn_id", "app turn")?;
                }
                let target = e.get("target").and_then(Value::as_str).unwrap_or("codex");
                if target != "codex" {
                    let who = target
                        .strip_prefix("claude.")
                        .ok_or_else(|| Error::new("invalid app delivery target"))?;
                    let p = member(&state, who)?;
                    if p.text("provider") != "claude"
                        || p.text("role") != "main"
                        || p.text("consumer").is_empty()
                    {
                        return Err(Error::new(
                            "Claude app delivery needs an enrolled native main",
                        ));
                    }
                }
                let prior = self.prior_publication(&state, target)?;
                if e.text("status") == "pending" {
                    if prior.as_ref().is_some_and(|p| {
                        !e.flag("retry")
                            && (p.text("status") == "pending"
                                || p.flag("delivery_unknown")
                                || p["digest"] == e["digest"] && p.text("status") == "persisted")
                    }) {
                        return Err(Error::new(
                            "snapshot already submitted; inspect its receipt before explicit retry",
                        ));
                    }
                } else if prior.as_ref().is_none_or(|p| {
                    p["id"] != e["id"]
                        || p["digest"] != e["digest"]
                        || p.text("status") != "pending"
                }) {
                    return Err(Error::new(
                        "publication receipt does not match its durable intent",
                    ));
                }
            }
            "join" | "main-resume" => {
                id(e, "participant", "participant")?;
                id(e, "provider", "provider")?;
                if self.services.canonical_provider(e.text("provider"))? != e.text("provider") {
                    return Err(Error::new("use a canonical registered provider"));
                }
                if !ROLES.contains(&e.text("role")) {
                    return Err(Error::new("invalid room role"));
                }
                if !e["model"].is_null() && !model_id(e.text("model")) {
                    return Err(Error::new("invalid room model"));
                }
                if e.text("label").is_empty() || e.text("label") != clean(e.text("label"), 120) {
                    return Err(Error::new("use a bounded plain participant label"));
                }
                if !e["consumer"].is_null() && !hex(e.text("consumer"), 32) {
                    return Err(Error::new("invalid native session binding"));
                }
                if !e.text("consumer").is_empty()
                    && state.list("participants").iter().any(|p| {
                        p.flag("joined")
                            && p["consumer"] == e["consumer"]
                            && p["participant"] != e["participant"]
                            && p["participant"] != e["replaces"]
                    })
                {
                    return Err(Error::new(
                        "native session already has a participant in this room",
                    ));
                }
                let scopes = e.get("owns").map(strings).transpose()?.unwrap_or_default();
                declared_scopes(e.text("role"), &scopes)?;
                let prior = state
                    .list("participants")
                    .iter()
                    .find(|p| p["participant"] == e["participant"]);
                if !e["task_id"].is_null() {
                    id(e, "task_id", "task")?;
                    if e.text("parent").is_empty() || e.text("role") != "worker" {
                        return Err(Error::new(
                            "write task declaration needs a worker and parent",
                        ));
                    }
                    if scopes.is_empty()
                        && !carry.active()
                        && !prior.is_some_and(|p| p.list("owns").is_empty())
                    {
                        return Err(Error::new("write task declaration needs an initial scope"));
                    }
                }
                if !e.text("parent").is_empty() {
                    member(&state, e.text("parent"))?;
                }
                if let Some(p) = prior {
                    if ["provider", "role", "model", "parent", "task_id"]
                        .iter()
                        .any(|k| p[k] != e[k])
                    {
                        return Err(Error::new(
                            "participant identity is immutable; use a new participant id",
                        ));
                    }
                    if p.flag("joined")
                        && e.as_object().is_some_and(|v| {
                            v.iter().all(|(k, v)| {
                                p.as_object().is_some_and(|fields| {
                                    fields
                                        .iter()
                                        .find(|(pk, _)| pk == k)
                                        .is_some_and(|(_, pv)| pv == v)
                                })
                            })
                        })
                    {
                        return Ok(false);
                    }
                    if p.flag("joined") {
                        return Err(Error::new("leave before changing participant metadata"));
                    }
                }
                if !e["replaces"].is_null() {
                    id(e, "replaces", "previous main")?;
                    if carry.active() || e.text("role") != "main" || prior.is_some() {
                        return Err(Error::new("native resume needs a fresh main participant"));
                    }
                }
                if !carry.active() && !e.text("parent").is_empty() && !scopes.is_empty() {
                    self.refuse_finished_call(room_id, rows, e.text("participant"))?;
                }
                if !e["task_id"].is_null() && !carry.active() {
                    let found = self.scope_overlaps(room_id, &state, e.text("parent"), &scopes)?;
                    if !found.is_empty() {
                        let detail=found.iter().map(|b|format!("task {} participant {} scope {}; wait for its result to be delivered or read, or for the declaration to be released",b.text("task_id"),b.text("participant"),b.text("scope"))).collect::<Vec<_>>().join("; ");
                        self.notices
                            .borrow_mut()
                            .push(format!("Scope overlap: {detail}"));
                        return Err(Error::new(
                            "Scope overlap: write dispatch refused; resolve the blockers listed above",
                        ));
                    }
                }
                if prior.is_none() {
                    let called = !e.text("parent").is_empty();
                    let peers = state
                        .list("participants")
                        .iter()
                        .filter(|p| !p.text("parent").is_empty() == called)
                        .count();
                    if peers >= if called { MAX_CALLS } else { MAX_PARTICIPANTS } {
                        return Err(Error::new(if called {
                            "room call limit reached; start a new bounded room"
                        } else {
                            "room participant limit reached"
                        }));
                    }
                }
                if e.text("role") == "main" && !carry.active() {
                    self.validate_new_main(rows, &state, e, proofs)?;
                }
            }
            "leave" => {
                let p = member(&state, e.text("participant"))?;
                if !carry.active()
                    && let Some(caller) = &self.session.participant
                {
                    if !p.text("parent").is_empty() {
                        if caller != p.text("parent") || !self.bound(room_id)? {
                            return Err(Error::new(format!(
                                "a call leaves through its parent main {}",
                                p.text("parent")
                            )));
                        }
                    } else if caller != e.text("participant") || !self.bound(room_id)? {
                        return Err(Error::new(
                            "leave must name this session's participant and room",
                        ));
                    }
                }
            }
            "call-result" => {
                for (k, label) in [
                    ("event_id", "event"),
                    ("task_id", "task"),
                    ("call_id", "call"),
                    ("attempt_id", "attempt"),
                ] {
                    id(e, k, label)?;
                }
                self.result_participant(&state, e.text("call_id"))?;
                for k in ["artifact", "patch"] {
                    if !e[k].is_null() && !safe_room_path(e.text(k)) {
                        return Err(Error::new("call result paths must be repository-relative"));
                    }
                }
                if !e["patch_sha256"].is_null() && !hex(e.text("patch_sha256"), 64) {
                    return Err(Error::new("invalid call result patch digest"));
                }
                if let Some(v) = e.get("changed_files")
                    && strings(v)?.iter().any(|p| !safe_room_path(p))
                {
                    return Err(Error::new("call result files must be repository-relative"));
                }
            }
            "review-dispatch" | "review-dispatch-failed" => {
                for (k, label) in [
                    ("event_id", "event"),
                    ("task_id", "task"),
                    ("call_id", "call"),
                    ("review_call_id", "call"),
                ] {
                    id(e, k, label)?;
                }
                self.result_participant(&state, e.text("call_id"))?;
                if !controller_review_ids(e) {
                    return Err(Error::new(
                        "automatic review ids do not match the reviewed call",
                    ));
                }
                let claims = structured
                    .iter()
                    .filter(|p| p.text("kind") == "review-dispatch" && same_review(p, e))
                    .collect::<Vec<_>>();
                let failures: BTreeSet<_> = structured
                    .iter()
                    .filter(|p| p.text("kind") == "review-dispatch-failed" && same_review(p, e))
                    .map(|p| p.number("attempt"))
                    .collect();
                if kind == "review-dispatch" {
                    let expected = claims.len() as u64 + 1;
                    if e.number("attempt") != expected
                        || expected > 2
                        || claims
                            .last()
                            .is_some_and(|p| !failures.contains(&p.number("attempt")))
                    {
                        return Err(Error::new("review patch digest already claimed"));
                    }
                } else if !claims.iter().any(|p| p["attempt"] == e["attempt"])
                    || failures.contains(&e.number("attempt"))
                {
                    return Err(Error::new(
                        "automatic review failure does not match an active claim",
                    ));
                }
            }
            "message" => {
                id(e, "id", "message")?;
                let sender = member(&state, e.text("sender"))?;
                let recipient = e.text("recipient");
                if recipient != "all" {
                    member(&state, recipient)?;
                }
                if recipient == e.text("sender") {
                    return Err(Error::new("a message needs another participant or all"));
                }
                if let Some(audience) = e.get("audience") {
                    let audience = strings(audience)?;
                    if recipient != "all"
                        || e.text("message_kind") == "status"
                        || audience.iter().any(|p| p == e.text("sender"))
                    {
                        return Err(Error::new("a carried audience belongs to a broadcast"));
                    }
                    for p in audience {
                        member(&state, &p)?;
                    }
                }
                if !KINDS.contains(&e.text("message_kind")) {
                    return Err(Error::new("invalid message kind"));
                }
                if e.text("id").starts_with("result-")
                    && (e.text("id") != format!("result-{}", e.text("sender"))
                        || e.text("message_kind") != "handoff"
                        || sender.text("parent") != recipient)
                {
                    return Err(Error::new(
                        "result-<participant> ids are reserved for that participant's handoff to its parent",
                    ));
                }
                if !carry.active()
                    && ["assign-", "dispatch-"].iter().any(|prefix| {
                        e.text("id")
                            .strip_prefix(prefix)
                            .is_some_and(crate::desktop::request_uuid)
                    })
                {
                    if !text_checked {
                        return Err(Error::new("assignment needs a complete unmodified request"));
                    }
                    let Carry::DesktopOwner(consumer) = carry else {
                        return Err(Error::new("assignment records require the bound owner"));
                    };
                    self.validate_desktop_assignment(&state, e, text, consumer)?;
                    let guard = self.operations()?;
                    for a in guard
                        .attempts()
                        .iter()
                        .filter(|a| a.terminal && a.tool == "panel-main")
                    {
                        if a.refs["panel_room_participant"]
                            .as_str()
                            .unwrap_or(&a.attempt_id)
                            == state.text("owner")
                            && self.same_room(a.refs.text("panel_room_id"), room_id)?
                        {
                            return Err(Error::new("conversation is not the owner of this Room"));
                        }
                    }
                    guard.check()?;
                    proofs.push(guard);
                }
                if e.text("id").starts_with("judgment-")
                    && e.text("id") != format!("judgment-{}", e.text("sender"))
                {
                    return Err(Error::new(
                        "judgment-<participant> ids are reserved for the panel's notice about that call",
                    ));
                }
                if e.flag("quiet")
                    && (e.text("id") != format!("result-{}", e.text("sender"))
                        || e.text("message_kind") != "handoff")
                {
                    return Err(Error::new("only a call's result handoff can be quiet"));
                }
                if e.text("id").starts_with("rollover-unread-") && !carry.active() {
                    return Err(Error::new(
                        "rollover-unread- ids are reserved for a rollover's note",
                    ));
                }
                let old = state.list("messages").iter().find(|m| m["id"] == e["id"]);
                if old.is_none() && state.list("messages").len() >= MAX_MESSAGES {
                    return Err(Error::new(
                        "room message limit reached; start a new bounded room",
                    ));
                }
                if let Some(old) = old {
                    if e.as_object().is_some_and(|v| {
                        v.iter().all(|(k, v)| {
                            old.as_object().is_some_and(|fields| {
                                fields
                                    .iter()
                                    .find(|(pk, _)| pk == k)
                                    .is_some_and(|(_, pv)| pv == v)
                            })
                        })
                    }) && old.text("text") == text
                    {
                        return Ok(false);
                    }
                    return Err(Error::new("message id conflicts with its existing content"));
                }
                if !carry.active()
                    && e.text("sender") == crate::desktop::PARTICIPANT
                    && e.text("message_kind") == "answer"
                {
                    self.validate_desktop_answer(&state, e, text)?;
                }
                if !e.text("reply_to").is_empty() {
                    let mut original = state
                        .list("messages")
                        .iter()
                        .find(|m| m.text("id") == e.text("reply_to"))
                        .cloned();
                    if carry.active()
                        && original.as_ref().is_some_and(|m| {
                            !m.list("targets")
                                .iter()
                                .any(|p| p.as_str() == Some(e.text("sender")))
                        })
                    {
                        original = self
                            .lineage_messages(
                                &state,
                                &BTreeSet::from([e.text("reply_to").to_owned()]),
                            )?
                            .remove(e.text("reply_to"));
                    }
                    if original.as_ref().is_none_or(|m| {
                        !m.list("targets")
                            .iter()
                            .any(|p| p.as_str() == Some(e.text("sender")))
                            || e["recipient"] != m["sender"]
                    }) {
                        return Err(Error::new(
                            "reply must return to the sender of a message addressed to this participant",
                        ));
                    }
                }
            }
            "received" => {
                member(&state, e.text("participant"))?;
                let mids = strings(&e["messages"])?;
                if mids.is_empty() || mids.len() > 200 {
                    return Err(Error::new("ack requires bounded message ids"));
                }
                let mut indexed: BTreeMap<String, Value> = state
                    .list("messages")
                    .iter()
                    .map(|m| (m.text("id").to_owned(), m.clone()))
                    .collect();
                let wanted = mids
                    .iter()
                    .filter(|m| !indexed.contains_key(*m))
                    .cloned()
                    .collect();
                for (mid, mut m) in self.lineage_messages(&state, &wanted)? {
                    let done = rows
                        .iter()
                        .map(|r| &r["room_event"])
                        .filter(|e| {
                            e.text("kind") == "received"
                                && e.list("messages")
                                    .iter()
                                    .any(|m| m.as_str() == Some(mid.as_str()))
                        })
                        .map(|e| e["participant"].clone())
                        .collect::<Vec<_>>();
                    m.set("received_by", done);
                    indexed.insert(mid, m);
                }
                for mid in &mids {
                    if indexed.get(mid).is_none_or(|m| {
                        !m.list("targets")
                            .iter()
                            .any(|p| p.as_str() == Some(e.text("participant")))
                    }) {
                        return Err(Error::new(
                            "cannot acknowledge a message addressed to another participant",
                        ));
                    }
                }
                if mids.iter().all(|mid| {
                    indexed[mid]
                        .list("received_by")
                        .iter()
                        .any(|p| p.as_str() == Some(e.text("participant")))
                }) {
                    return Ok(false);
                }
            }
            _ => return Err(Error::new("invalid room event")),
        }
        Ok(true)
    }
    fn refuse_finished_call(&self, id: &str, rows: &[Value], who: &str) -> Result<()> {
        if self.status_rows(id, rows)?["call_results"]
            .get(who)
            .is_some()
        {
            Err(Error::new(
                "a finished write call id is single-use; dispatch a new call",
            ))
        } else {
            Ok(())
        }
    }
    pub fn validate_main_swap(
        &self,
        state: &Value,
        e: &Value,
        old: &Attempt,
        released: bool,
    ) -> Result<()> {
        let p = member(state, e.text("participant"))?;
        if p.text("role") != "main" || !p.text("parent").is_empty() {
            return Err(Error::new("swap needs the current main"));
        }
        if !["claude", "codex", "pi", "agy"].contains(&e.text("harness"))
            || !main_owner_matches(e.text("harness"), e.text("provider"))
            || !model_id(e.text("model"))
            || e.text("model").starts_with('-')
        {
            return Err(Error::new("invalid swap provider or model"));
        }
        if !e
            .text("attempt")
            .strip_prefix("att_")
            .is_some_and(|v| hex(v, 32))
        {
            return Err(Error::new("invalid swapped main attempt"));
        }
        let refs = &old.refs;
        if refs.as_object().is_none() {
            return Err(Error::new("swapped main ownership is malformed"));
        }
        let owner = refs
            .get("panel_owner")
            .and_then(Value::as_str)
            .unwrap_or(&old.provider);
        let harness = refs
            .get("panel_harness")
            .and_then(Value::as_str)
            .unwrap_or(&old.provider);
        if old.tool != "panel-main"
            || old.provider != p.text("provider")
            || owner != old.provider
            || !main_owner_matches(harness, &old.provider)
            || refs.text("panel_harness") == e.text("harness") && p["model"] == e["model"]
            || old.state != if released { "abandoned" } else { "working" }
            || old.terminal != released
            || released && old.reason_code.as_deref() != Some("main_swapped")
            || refs
                .get("panel_room_participant")
                .and_then(Value::as_str)
                .unwrap_or(&old.attempt_id)
                != e.text("participant")
            || old.attempt_id != e.text("attempt")
            || !self.same_room(refs.text("panel_room_id"), state.text("id"))?
        {
            return Err(Error::new(if released {
                "swapped main release is unverified"
            } else {
                "current main ownership is unverified"
            }));
        }
        Ok(())
    }
    fn enrollment(
        &self,
        rows: &[Value],
        state: &Value,
        p: &Value,
    ) -> Result<Option<(String, Value)>> {
        let mut rows = rows.to_vec();
        let mut state = state.clone();
        let mut p = p.clone();
        for old in self.predecessors(state.text("id"), true)? {
            let copied = rows
                .iter()
                .find(|r| r["room_event"].text("kind") == "carried")
                .map(|r| r.number("seq"))
                .unwrap_or(0);
            if state["predecessor"].is_null() || p.number("seq") > copied {
                break;
            }
            rows = self.snapshot(&old)?.rows.clone();
            state = project(&rows)?;
            let Some(previous) = state.list("participants").iter().find(|m| {
                m["participant"] == p["participant"] && m.flag("joined") && m.text("role") == "main"
            }) else {
                return Ok(None);
            };
            p = previous.clone();
        }
        let copied = rows
            .iter()
            .find(|r| r["room_event"].text("kind") == "carried")
            .map(|r| r.number("seq"))
            .unwrap_or(0);
        if !state["predecessor"].is_null() && p.number("seq") <= copied {
            return Ok(None);
        }
        Ok(Some((state.text("id").to_owned(), p)))
    }
    fn validate_new_main<'s>(
        &'s self,
        rows: &[Value],
        state: &Value,
        event: &Value,
        proofs: &mut Vec<Box<dyn OperationsRead + 's>>,
    ) -> Result<()> {
        let others = state
            .list("participants")
            .iter()
            .filter(|p| {
                p.flag("joined")
                    && p.text("role") == "main"
                    && p["participant"] != event["participant"]
            })
            .collect::<Vec<_>>();
        if others.is_empty() && event["replaces"].is_null() {
            return Ok(());
        }
        let read = self.operations()?;
        if !event["replaces"].is_null() {
            let previous = member(state, event.text("replaces"))?;
            if state
                .list("participants")
                .iter()
                .any(|p| p.flag("joined") && p["parent"] == previous["participant"])
            {
                return Err(Error::new(
                    "native resume has joined descendants; reconcile them before replacing the main",
                ));
            }
            if previous.text("role") != "main"
                || !previous.text("parent").is_empty()
                || !event.text("parent").is_empty()
                || previous["provider"] != event["provider"]
                || previous.text("consumer").is_empty()
                || previous["consumer"] != event["consumer"]
            {
                return Err(Error::new(
                    "native resume binding changed or belongs to another main",
                ));
            }
            let exact = |who: &str, terminal: bool| {
                read.attempts().iter().position(|a| {
                    a.attempt_id == who
                        && a.tool == "panel-main"
                        && a.provider == event.text("provider")
                        && a.terminal == terminal
                        && (terminal || a.state == "working")
                        && a.refs.text("panel_role") == "main"
                        && a.refs.text("panel_owner") == a.provider
                        && main_owner_matches(a.refs.text("panel_harness"), &a.provider)
                        && a.refs
                            .get("panel_room_participant")
                            .and_then(Value::as_str)
                            .unwrap_or(&a.attempt_id)
                            == who
                        && self
                            .same_room(a.refs.text("panel_room_id"), state.text("id"))
                            .unwrap_or(false)
                        && a.refs.text("panel_session_digest") == event.text("consumer")
                })
            };
            let old = exact(event.text("replaces"), true);
            let new = exact(event.text("participant"), false);
            if !old.zip(new).is_some_and(|(old, new)| old < new) {
                return Err(Error::new(
                    "native resume main ownership or terminal release is unverified",
                ));
            }
        }
        let mut here: BTreeSet<String> = self
            .predecessors(state.text("id"), true)?
            .into_iter()
            .collect();
        here.insert(state.text("id").to_owned());
        for other in others {
            let related = read
                .attempts()
                .iter()
                .filter(|a| {
                    a.tool == "panel-main"
                        && a.refs.text("panel_role") == "main"
                        && here.contains(a.refs.text("panel_room_id"))
                        && a.refs
                            .get("panel_room_participant")
                            .and_then(Value::as_str)
                            .unwrap_or(&a.attempt_id)
                            == other.text("participant")
                })
                .collect::<Vec<_>>();
            let enrollment = self.enrollment(rows, state, other)?;
            let current = if let Some((started, p)) = enrollment {
                related.iter().any(|a| {
                    let bound = p["seq"] == p["initial_seq"]
                        && a.attempt_id == p.text("participant")
                        && a.refs.text("panel_room_id") == started;
                    let newer =
                        chrono::NaiveDateTime::parse_from_str(&a.created_at, "%Y-%m-%dT%H:%M:%SZ")
                            .ok()
                            .zip(
                                chrono::NaiveDateTime::parse_from_str(
                                    p.text("joined_at"),
                                    "%Y-%m-%dT%H:%M:%SZ",
                                )
                                .ok(),
                            )
                            .is_some_and(|(a, b)| a > b);
                    bound || newer
                })
            } else {
                false
            };
            if !current || related.iter().any(|a| !a.terminal) {
                return Err(Error::new(format!(
                    "main {} is live or its release is unverified; use oms panel --swap-main PROVIDER to hand off the main",
                    other.text("participant")
                )));
            }
        }
        read.check()?;
        proofs.push(read);
        Ok(())
    }
}
