//! A desktop decision answers one immutable question addressed by the Room owner.
//! The reply goes into the owner's ordinary inbox. Privileged operation grants
//! keep their separate one-use approval contract.
use crate::core::Carry;
use crate::model::{hex, identifier};
use crate::value::{Value, encode, obj};
use crate::{Error, Result, Room};

pub(crate) const PARTICIPANT: &str = "oms-desktop";

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
