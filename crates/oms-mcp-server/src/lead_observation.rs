//! An observation belongs to one completed provider invocation. Only its
//! protected Room receipt may select the operation. Never infer a native
//! conversation handle from answer text, the account's history or a route.
use super::{Conversation, assignment_address, checked_peer_directory};
use crate::{Server, storage, value::Value};
use rustix::fs::{AtFlags, FileType, statat};

impl Conversation<'_> {
    pub(super) fn native_invocation(&self, server: &Server<'_>) -> Option<(Value, Value)> {
        let provider = match self.provider {
            "claude" => "claude",
            "agy" => "antigravity",
            _ => return None,
        };
        // Show only the latest request. An earlier success must not populate
        // a pending, unobserved or failed turn's settings.
        let head = self.head().ok()?;
        let record = match &head {
            Some(id) => Some(self.record(id).ok()??),
            None => None,
        };
        let (binding, message) = match &record {
            Some(r) => (
                r["binding"].as_str()?,
                Some((r["digest"].as_str()?, &r["route"])),
            ),
            None => (self.scope, None),
        };
        let op = assignment_address(self.lead, binding)?;
        let directory = || {
            checked_peer_directory(
                self.repo,
                self.lead,
                &op,
                self.thread,
                binding,
                true,
                message,
            )
            .ok()
            .flatten()
        };
        let pin = storage::Directory::run(self.repo, &op, true).ok()?;
        let identity = |opened: &storage::Directory| {
            rustix::fs::fstat(&opened.fd)
                .ok()
                .map(|s| (s.st_dev, s.st_ino, s.st_mode))
        };
        let dir = directory()?;
        if crate::peer::status(&dir) != ("done", Some(0)) {
            return None;
        }
        const RECEIPT: &str = "native-lead-observation.json";
        let rel = format!("{}/{op}/answers", crate::RUN_ROOT);
        let answers = storage::Directory::open(self.repo, &rel, false, true).ok()?;
        let before = statat(&answers.fd, RECEIPT, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
            || before.st_mode & 0o077 != 0
        {
            return None;
        }
        let root = oms_safe_fs::Root::open(self.repo).ok()?;
        let read = |file: &str, limit| {
            root.read(
                &format!("{rel}/{file}"),
                limit,
                oms_safe_fs::Policy::manifest(),
            )
            .ok()
        };
        let bytes = read(RECEIPT, 2048)?;
        oms_context::parse_json_bounded(&bytes, 2048).ok()?;
        let row = crate::value::from_slice(&bytes).ok()?;
        if row["schema"] != 1 || row["operation"] != op || row["provider"] != provider {
            return None;
        }
        let id = row["sessionId"].as_str()?;
        if !super::super::uuid(id)
            || !(b'1'..=b'8').contains(&id.as_bytes()[14])
            || !matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
        {
            return None;
        }
        let served = match row.get("model")? {
            Value::Null => Value::Null,
            v => {
                let model = v.as_str()?;
                if provider != "claude"
                    || !crate::peer::valid_lead_route(provider, model, None)
                    || crate::output::outbound(model.as_bytes(), &Value::Null).is_err()
                {
                    return None;
                }
                json!({"model":model,"effort":Value::Null})
            }
        };
        let result = server
            .peer_result(&json!({"repo":self.repo,"operation":op}))
            .ok()?;
        let result = crate::value::from_str(result.as_str()?).ok()?;
        let [answer] = result["answer_details"].as_array()?.as_slice() else {
            return None;
        };
        let artifact = row["artifact"].as_str().filter(|p| crate::answer_name(p))?;
        if result["operation"] != op
            || result["exit"] != 0
            || answer["exit"] != "0"
            || answer["read_arguments"]["operation"] != op
            || answer["read_arguments"]["answer_ref"]
                .as_str()?
                .rsplit_once(':')?
                .0
                != artifact
        {
            return None;
        }
        let raw = read(artifact, crate::MAX_ANSWER)?;
        if row["artifactSha256"] != storage::hash(&raw) {
            return None;
        }
        let (text, exit) = storage::sections(&raw);
        if exit != "0"
            || text.is_empty()
            || answer["answer_sha256"] != storage::hash(&text)
            || self.head().ok()? != head
        {
            return None;
        }
        let current = storage::Directory::run(self.repo, &op, true).ok()?;
        if identity(&pin)? != identity(&current)?
            || crate::peer::status(&directory()?) != ("done", Some(0))
            || identity(&answers)?
                != identity(&storage::Directory::open(self.repo, &rel, false, true).ok()?)?
        {
            return None;
        }
        let after = statat(&answers.fd, RECEIPT, AtFlags::SYMLINK_NOFOLLOW).ok()?;
        if (after.st_dev, after.st_ino, after.st_mode)
            != (before.st_dev, before.st_ino, before.st_mode)
            || read(RECEIPT, 2048)? != bytes
        {
            return None;
        }
        Some((
            json!({"id":id,"scope":"invocation","source":"provider-result","resumable":false}),
            served,
        ))
    }
}
