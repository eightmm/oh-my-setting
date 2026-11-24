//! A desktop decision answers one immutable question addressed by the Room owner.
//! The reply goes into the owner's ordinary inbox. Privileged operation grants
//! keep their separate one-use approval contract.
use crate::core::Carry;
use crate::model::{clean, hex, identifier, member};
use crate::value::{Value, encode, obj};
use crate::{Error, Result, Room};

pub(crate) const PARTICIPANT: &str = "oms-desktop";

/// An existing panel-main lifecycle row for read-only conversation matching.
pub struct ConversationPanelAttempt<'a> {
    pub room: &'a str,
    pub participant: &'a str,
    pub terminal: bool,
}

impl Room<'_> {
    fn desktop_request(&self, room: &Value, question: &Value) -> Result<Option<Value>> {
        let owner = room.text("owner");
        let enrolled = |id| {
            room.list("participants")
                .iter()
                .find(|p| p.text("participant") == id && p.flag("joined"))
        };
        let Some(main) = enrolled(owner) else {
            return Ok(None);
        };
        if room.flag("closed")
            || room.flag("sealed")
            || main.text("role") != "main"
            || enrolled(PARTICIPANT).is_none_or(|p| p.text("role") != "advisor")
            || question.text("recipient") != PARTICIPANT
            || question.text("sender") != owner
            || question.text("message_kind") != "question"
            || question.flag("answered")
            || question
                .text("text")
                .starts_with("[earlier output truncated: see artifact]")
            || main.number("seq") > question.number("seq")
        {
            return Ok(None);
        }
        let id = format!("{}/{}", room.text("id"), question.text("id"));
        let mut item = obj([
            ("id", id.into()),
            ("version", question["seq"].clone()),
            ("object", room["id"].clone()),
            ("action", "Main scope decision".into()),
            ("sender", owner.into()),
            ("summary", question["text"].clone()),
        ]);
        let binding = obj([
            ("repo", crate::value::os_value(self.repo.as_os_str())),
            ("request", item.clone()),
        ]);
        item.set("digest", crate::digest(&encode(&binding, false, true)?));
        Ok(Some(item))
    }

    /// Read a positive native-session membership. Unlike mail routing, this
    /// observation grants no authority and need not claim that the Room is unique.
    pub fn conversation_main<'a>(
        &self,
        consumer: &str,
        panel_mains: impl IntoIterator<Item = ConversationPanelAttempt<'a>>,
    ) -> Result<Value> {
        if !hex(consumer, 32) {
            return Err(Error::new("invalid conversation binding digest"));
        }
        let (mut rooms, incomplete) = self.discover()?;
        // The pointer only hints at another log when bounded discovery omits it.
        // Verify its participant, owner and native binding exactly as for every candidate.
        if incomplete
            && let Some(store) = &self.store
            && let Ok((raw, _)) = store.threads.read(std::ffi::OsStr::new("CURRENT"), 4096)
            && let Some(id) = std::str::from_utf8(&raw)
                .ok()
                .and_then(|v| v.split_whitespace().next())
            && identifier(id, "room").is_ok()
            && !rooms.iter().any(|r| r.text("id") == id)
            && let Ok(state) = self.records(id).and_then(|rows| crate::project(&rows))
            && self.published_room(&state).unwrap_or(false)
        {
            rooms.push(state);
        }
        self.match_conversation_main(consumer, panel_mains, &rooms, incomplete)
    }

    fn match_conversation_main<'a>(
        &self,
        consumer: &str,
        panel_mains: impl IntoIterator<Item = ConversationPanelAttempt<'a>>,
        rooms: &[Value],
        incomplete: bool,
    ) -> Result<Value> {
        let mut result = obj([
            ("conversationMain", Value::Null),
            ("conversationBinding", "unconfirmed".into()),
        ]);
        let matches = |r: &&Value| {
            !r.flag("closed")
                && !r.flag("sealed")
                && r["successor"].is_null()
                && r.list("participants").iter().any(|p| {
                    p.flag("joined")
                        && p.text("provider") == "codex"
                        && p.text("role") == "main"
                        && p.text("participant") == r.text("owner")
                        && p.text("consumer") == consumer
                })
        };
        let mut candidates = rooms.iter().filter(matches);
        let Some(room) = candidates.next() else {
            return Ok(result);
        };
        if candidates.next().is_some() {
            result.set("conversationBinding", "ambiguous");
            return Ok(result);
        }
        // A terminal panel attempt supersedes its Room's still-joined display
        // membership. Read the full producer projection, outside the dashboard's recent window.
        for attempt in panel_mains {
            if attempt.terminal
                && attempt.participant == room.text("owner")
                && self.same_room(attempt.room, room.text("id"))?
            {
                return Ok(result);
            }
        }
        result.set("conversationBinding", "matched");
        result.set(
            "conversationMain",
            obj([
                ("provider", "codex".into()),
                ("room", room["id"].clone()),
                ("participant", room["owner"].clone()),
                ("source", "room-session".into()),
                (
                    "participants",
                    room.list("participants")
                        .iter()
                        .take(32)
                        .map(|p| {
                            obj([
                                ("room", room["id"].clone()),
                                ("participant", p["participant"].clone()),
                                ("role", p["role"].clone()),
                                ("provider", p["provider"].clone()),
                                ("joined", p["joined"].clone()),
                            ])
                        })
                        .collect::<Vec<_>>()
                        .into(),
                ),
                (
                    "participantsOmitted",
                    (room.list("participants").len().saturating_sub(32) as u64).into(),
                ),
                ("state", "joined".into()),
                ("incomplete", incomplete.into()),
            ]),
        );
        Ok(result)
    }

    /// Read an exact published Room. Only rows shared with this native Main and
    /// involving the selected participant can cross into its conversation tab.
    pub fn desktop_chat(
        &self,
        id: &str,
        who: &str,
        consumer: &str,
        before: Option<u64>,
    ) -> Result<Value> {
        identifier(id, "room")?;
        identifier(who, "participant")?;
        if self.session.child
            || self.session.participant.is_some()
            || !hex(consumer, 32)
            || before == Some(0)
        {
            return Err(Error::new("desktop chat needs a bound app conversation"));
        }
        let room = crate::project(&self.records(id)?)?;
        let owner = room.text("owner");
        let viewer = crate::model::member(&room, owner)?;
        if room.text("id") != id
            || room.flag("closed")
            || room.flag("sealed")
            || !room["successor"].is_null()
            || !self.published_room(&room)?
            || viewer.text("consumer") != consumer
            || viewer.text("provider") != "codex"
            || viewer.text("role") != "main"
        {
            return Err(Error::new("conversation is not the owner of this Room"));
        }
        let member = room
            .list("participants")
            .iter()
            .find(|p| p.text("participant") == who)
            .ok_or_else(|| Error::new("participant is not recorded in this Room"))?;
        let mut messages = Vec::new();
        let mut next = Value::Null;
        let mut bytes = 0;
        for m in room.list("messages").iter().rev() {
            let seq = m["seq"]
                .as_u64()
                .ok_or_else(|| Error::new("invalid room message sequence"))?;
            let visible_to = |participant: &str| {
                m.text("sender") == participant
                    || m.text("recipient") == participant
                    || (m.text("recipient") == "all"
                        && m.list("targets")
                            .iter()
                            .any(|target| target.as_str() == Some(participant)))
            };
            if seq < viewer.number("seq")
                || before.is_some_and(|n| seq >= n)
                || !visible_to(owner)
                || !visible_to(who)
            {
                continue;
            }
            let item = obj([
                ("seq", m["seq"].clone()),
                ("at", m["ts"].clone()),
                ("from", m["sender"].clone()),
                ("to", m["recipient"].clone()),
                ("kind", m["message_kind"].clone()),
                ("text", m["text"].clone()),
            ]);
            bytes += encode(&item, false, true)?.len();
            if messages.len() >= 12 || (!messages.is_empty() && bytes > 16_000) {
                next = messages
                    .last()
                    .map(|m: &Value| m["seq"].clone())
                    .unwrap_or(Value::Null);
                break;
            }
            messages.push(item);
        }
        messages.reverse();
        Ok(obj([
            ("room", id.into()),
            ("participant", who.into()),
            ("provider", member["provider"].clone()),
            ("source", "room".into()),
            ("self", (owner == who).into()),
            ("messages", messages.into()),
            ("nextBefore", next),
        ]))
    }

    pub(crate) fn desktop_start_status(&self, room: &str) -> Result<Value> {
        let request = identifier(room, "room")?
            .strip_prefix("oms-app-")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new("expected a desktop request Room"))?;
        let mut receipt = obj([
            ("requestId", request.into()),
            ("room", Value::Null),
            ("status", "not_found".into()),
            ("attempt", Value::Null),
            ("provider", Value::Null),
            ("state", Value::Null),
            ("terminal", Value::Null),
        ]);
        let Some(store) = &self.store else {
            return Ok(receipt);
        };
        store.check()?;
        if !store.threads.exists(&crate::storage::Store::name(room))? {
            return Ok(receipt);
        }
        let reserved = crate::project(&self.records(room)?)?;
        if reserved.text("id") != room {
            return Err(Error::new("desktop request Room differs"));
        }
        receipt.set("room", reserved["id"].clone());
        receipt.set("status", "reserved");
        // The full producer projection retains creation order. A later Main
        // succession in this Room must not replace the original start receipt.
        let operations = self.operations()?;
        if let Some(main) = operations
            .attempts()
            .iter()
            .find(|a| a.tool == "panel-main" && a.refs.text("panel_room_id") == room)
        {
            let harness = main.refs.text("panel_harness");
            if main.refs.text("panel_role") != "main"
                || !crate::main_owner_matches(harness, &main.provider)
            {
                return Err(Error::new("inconsistent desktop Main lifecycle"));
            }
            receipt.set("status", "recorded");
            receipt.set("attempt", main.attempt_id.as_str());
            receipt.set("provider", harness);
            receipt.set("state", main.state.as_str());
            receipt.set("terminal", main.terminal);
        }
        operations.check()?;
        Ok(receipt)
    }

    pub(crate) fn desktop_requests(&self) -> Result<Value> {
        let (rooms, mut incomplete) = self.discover()?;
        let mut requests = Vec::new();
        for room in &rooms {
            for question in room.list("messages") {
                if let Some(item) = self.desktop_request(room, question)? {
                    if requests.len() == 32 {
                        incomplete = true;
                        break;
                    }
                    requests.push(item);
                }
            }
        }
        Ok(obj([
            ("approvals", requests.into()),
            ("incomplete", incomplete.into()),
        ]))
    }

    pub(crate) fn desktop_decide(
        &self,
        room: &str,
        question: &str,
        version: &str,
        digest: &str,
        decision: &str,
    ) -> Result<Value> {
        identifier(room, "room")?;
        identifier(question, "question")?;
        if self.session.child
            || self.session.participant.is_some()
            || !hex(digest, 64)
            || !matches!(decision, "approve" | "deny")
        {
            return Err(Error::new(
                "desktop decision needs the parent app and an exact reviewed request",
            ));
        }
        let state = crate::project(&self.records(room)?)?;
        let request = state
            .list("messages")
            .iter()
            .find(|m| m.text("id") == question)
            .ok_or_else(|| Error::new("the reviewed question is unavailable"))?;
        if request.number("seq").to_string() != version {
            return Err(Error::new("question version changed"));
        }
        let event = obj([
            ("kind", "message".into()),
            ("sender", PARTICIPANT.into()),
            ("recipient", request["sender"].clone()),
            ("message_kind", "answer".into()),
            ("reply_to", question.into()),
            (
                "id",
                format!("oms-decision-{}", crate::digest(question.as_bytes())).into(),
            ),
        ]);
        let text = format!(
            "{} for the exact requested scope. Request digest {digest}",
            if decision == "approve" {
                "Approved"
            } else {
                "Denied"
            }
        );
        // No forwarding across rollover. The validator rechecks owner, question
        // and digest under the append lock; a stable reply id fences two clicks.
        self.append_at(room, &event, &text, Carry::No)?;
        Ok(obj([
            ("decision", decision.into()),
            ("id", format!("{room}/{question}").into()),
            ("digest", digest.into()),
        ]))
    }

    pub(crate) fn validate_desktop_answer(
        &self,
        state: &Value,
        event: &Value,
        text: &str,
    ) -> Result<()> {
        let question = state
            .list("messages")
            .iter()
            .find(|m| m.text("id") == event.text("reply_to"));
        if let Some(question) = question
            && let Some(item) = self.desktop_request(state, question)?
            && !self.session.child
            && self.session.participant.is_none()
            && event.text("recipient") == question.text("sender")
            && event.text("id")
                == format!(
                    "oms-decision-{}",
                    crate::digest(question.text("id").as_bytes())
                )
            && ["Approved", "Denied"].iter().any(|verb| {
                text == format!(
                    "{verb} for the exact requested scope. Request digest {}",
                    item.text("digest")
                )
            })
        {
            return Ok(());
        }
        Err(Error::new("stale or mismatched desktop approval request"))
    }
}

pub(crate) fn request_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                matches!(b, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

impl Room<'_> {
    /// The unique, joined, native Main of this exact Room, from the app
    /// connection's bound conversation only. A worker or participant session
    /// never qualifies.
    fn assignment_owner(&self, id: &str, consumer: &str, request: &str) -> Result<Value> {
        identifier(id, "room")?;
        if self.session.child
            || self.session.participant.is_some()
            || !hex(consumer, 32)
            || !request_uuid(request)
        {
            return Err(Error::new(
                "assignment needs a bound app conversation and a request UUID",
            ));
        }
        let operations = self.operations()?;
        let attempts = || {
            operations
                .attempts()
                .iter()
                .filter(|a| a.tool == "panel-main")
                .map(|a| ConversationPanelAttempt {
                    room: a.refs.text("panel_room_id"),
                    participant: a.refs["panel_room_participant"]
                        .as_str()
                        .unwrap_or(&a.attempt_id),
                    terminal: a.terminal,
                })
        };
        let same_owner = |found: &Value| {
            found.text("conversationBinding") == "matched"
                && found["conversationMain"].text("room") == id
        };
        let mut found = self.conversation_main(consumer, attempts())?;
        if same_owner(&found) && found["conversationMain"].flag("incomplete") {
            // Spend the larger budget only to recheck this positively observed Room.
            // A pointer or an incomplete second census cannot establish uniqueness.
            let (rooms, incomplete) = self.discover_with_byte_limit(2 * crate::MAX_SCAN_BYTES)?;
            if incomplete {
                return Err(Error::new("room_inventory_incomplete"));
            }
            found = self.match_conversation_main(consumer, attempts(), &rooms, false)?;
        }
        operations.check()?;
        if !same_owner(&found) {
            return Err(Error::new("conversation is not the owner of this Room"));
        }
        let room = crate::project(&self.records(id)?)?;
        let viewer = member(&room, room.text("owner"))?;
        if room.text("id") != id
            || room.flag("closed")
            || room.flag("sealed")
            || !room["successor"].is_null()
            || !self.published_room(&room)?
            || viewer.text("consumer") != consumer
            || viewer.text("provider") != "codex"
            || viewer.text("role") != "main"
        {
            return Err(Error::new("conversation is not the owner of this Room"));
        }
        Ok(room)
    }

    fn assignment_receipt(&self, room: &Value, request: &str, fresh: Option<bool>) -> Value {
        let who = format!("ask-{request}");
        let messages = room.list("messages");
        let find = |id: String| messages.iter().find(|m| m.text("id") == id);
        let Some(task) = find(format!("assign-{request}")) else {
            return obj([("request", request.into()), ("status", "not_found".into())]);
        };
        let member = room
            .list("participants")
            .iter()
            .find(|p| p.text("participant") == who);
        let mut out = obj([
            ("request", request.into()),
            ("room", room["id"].clone()),
            ("participant", who.as_str().into()),
            (
                "provider",
                member.map_or(Value::Null, |p| p["provider"].clone()),
            ),
            ("role", member.map_or(Value::Null, |p| p["role"].clone())),
            ("title", member.map_or(Value::Null, |p| p["label"].clone())),
            ("task", task["text"].clone()),
            ("assignedAt", task["ts"].clone()),
            ("readOnly", true.into()),
            ("thread", Value::Null),
            ("status", "assigned".into()),
            ("operation", Value::Null),
        ]);
        if let Some(receipt) = find(format!("dispatch-{request}")) {
            let Some(operation) = receipt.text("text").strip_prefix("operation ") else {
                return out;
            };
            if receipt.text("sender") != room.text("owner")
                || receipt.text("recipient") != who
                || receipt.text("message_kind") != "status"
                || identifier(operation, "operation").is_err()
            {
                return out;
            }
            out.set("thread", operation);
            out.set("status", "started");
            out.set("operation", operation);
        }
        if let Some(fresh) = fresh {
            out.set("fresh", fresh);
        }
        out
    }

    /// Record one durable read-only provider assignment. `fresh` is true only
    /// for the call that wrote the intent; every retry observes it instead.
    pub(crate) fn desktop_assign(
        &self,
        id: &str,
        consumer: &str,
        request: &str,
        provider: &str,
        title: &str,
        task: &str,
    ) -> Result<Value> {
        let room = self.assignment_owner(id, consumer, request)?;
        let provider = self.services.canonical_provider(provider)?;
        if !matches!(provider.as_str(), "claude" | "antigravity")
            || title.is_empty()
            || title != clean(title, 120)
            || task.trim().is_empty()
        {
            return Err(Error::new(
                "assignment needs a supported provider, title and task",
            ));
        }
        let owner = room.text("owner").to_owned();
        let who = format!("ask-{request}");
        let join = obj([
            ("kind", "join".into()),
            ("participant", who.as_str().into()),
            ("provider", provider.as_str().into()),
            ("role", "researcher".into()),
            ("model", Value::Null),
            ("label", title.into()),
            ("owns", Vec::<Value>::new().into()),
            ("parent", owner.as_str().into()),
        ]);
        let event = obj([
            ("kind", "message".into()),
            ("id", format!("assign-{request}").into()),
            ("sender", owner.as_str().into()),
            ("recipient", who.as_str().into()),
            ("message_kind", "handoff".into()),
        ]);
        let applied = self.append_batch_at(
            id,
            &[
                (&join, &format!("Joined: {title}"), Carry::No),
                (&event, task, Carry::DesktopOwner(consumer)),
            ],
        )?;
        let state = crate::project(&self.records(id)?)?;
        Ok(self.assignment_receipt(&state, request, Some(applied[1])))
    }

    pub(crate) fn desktop_assign_status(
        &self,
        id: &str,
        consumer: &str,
        request: &str,
    ) -> Result<Value> {
        let state = self.assignment_owner(id, consumer, request)?;
        Ok(self.assignment_receipt(&state, request, None))
    }

    /// Bind the peer operation the provider launch actually returned.
    pub(crate) fn desktop_assign_receipt(
        &self,
        id: &str,
        consumer: &str,
        request: &str,
        operation: &str,
    ) -> Result<Value> {
        let room = self.assignment_owner(id, consumer, request)?;
        identifier(operation, "operation")?;
        let event = obj([
            ("kind", "message".into()),
            ("id", format!("dispatch-{request}").into()),
            ("sender", room["owner"].clone()),
            ("recipient", format!("ask-{request}").into()),
            ("message_kind", "status".into()),
        ]);
        let text = format!("operation {operation}");
        self.append_at(id, &event, &text, Carry::DesktopOwner(consumer))?;
        let state = crate::project(&self.records(id)?)?;
        Ok(self.assignment_receipt(&state, request, None))
    }

    /// `assign-` and `dispatch-` ids are the only trusted association between
    /// a Room and a provider thread, so no participant or worker may write them.
    pub(crate) fn validate_desktop_assignment(
        &self,
        state: &Value,
        e: &Value,
        text: &str,
        consumer: &str,
    ) -> Result<()> {
        let id = e.text("id");
        let (dispatch, request) = match id.strip_prefix("assign-") {
            Some(r) => (false, r),
            None => (true, id.strip_prefix("dispatch-").unwrap_or("")),
        };
        let who = format!("ask-{request}");
        let owner = state.text("owner");
        let ok = request_uuid(request)
            && !self.session.child
            && self.session.participant.is_none()
            && member(state, owner).is_ok_and(|p| {
                p.text("consumer") == consumer
                    && p.text("role") == "main"
                    && p.text("provider") == "codex"
            })
            && e.text("sender") == owner
            && e.text("recipient") == who
            && member(state, &who)
                .is_ok_and(|p| p.text("role") == "researcher" && p.text("parent") == owner)
            && if dispatch {
                e.text("message_kind") == "status"
                    && state
                        .list("messages")
                        .iter()
                        .any(|m| m.text("id") == format!("assign-{request}"))
                    && matches!(
                        text.split_whitespace().collect::<Vec<_>>()[..],
                        ["operation", op] if identifier(op, "operation").is_ok()
                    )
            } else {
                e.text("message_kind") == "handoff"
            };
        if ok {
            Ok(())
        } else {
            Err(Error::new(
                "assignment records belong to the Room owner's app conversation",
            ))
        }
    }
}
