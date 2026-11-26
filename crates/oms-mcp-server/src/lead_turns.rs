//! User messages in an existing Room-bound OMS Lead conversation. The original
//! private operation owns the index. Each turn has one immutable reservation
//! and a distinct peer operation, even when the same text is sent twice.
use super::{PanelResult, SEND_LEAD, assignment_address, checked_peer_directory, uuid};
use crate::storage::{Directory, byte_prefix, hash};
use crate::value::Value;
use crate::{RUN_ROOT, Server};
use rustix::fs::{self, AtFlags, FileType};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

#[path = "lead_observation.rs"]
mod observation;

#[path = "lead_settings.rs"]
mod settings;

const INVALID: &str = "The Lead conversation receipt is unavailable or changed.";
const HEAD: &str = "lead-turn-head.json";
const PAGE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TurnLookupError {
    OutboundWithheld,
    InvalidRecord,
    ReceiptChanged,
}

impl TurnLookupError {
    pub fn marker(self) -> Value {
        let (status, reason) = match self {
            Self::OutboundWithheld => ("unavailable", "outbound_withheld"),
            Self::InvalidRecord => ("unavailable", "invalid_turn_record"),
            Self::ReceiptChanged => ("unconfirmed", "receipt_changed"),
        };
        json!({"status":status,"reason":reason})
    }
}

type CachedTurns = BTreeMap<(String, String), (String, Result<&'static str, TurnLookupError>)>;

#[derive(Clone, Default)]
pub(crate) struct LeadTurnCache(Arc<Mutex<CachedTurns>>);

fn turn_id(id: &str) -> bool {
    uuid(id) && id.as_bytes()[14] == b'4' && matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
}

fn text_ok(s: &str) -> bool {
    (1..=4000).contains(&s.len())
        && !s.trim().is_empty()
        && !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

pub(super) fn validate(name: &str, args: &Value, provider: Option<&str>) -> PanelResult<()> {
    for key in ["before", "turnId"] {
        if let Some(v) = args.get(key)
            && (provider.is_none() || !v.as_str().is_some_and(turn_id))
        {
            return Err("Select a Lead and supply a valid message ID or page cursor.".into());
        }
    }
    if let Some(cursor) = args.get("answerPage") {
        if provider.is_none()
            || !args["projectKey"].is_string()
            || !args["leadRequestId"].as_str().is_some_and(uuid)
            || args.get("turnId").is_none()
            || args.get("before").is_some()
        {
            return Err("A full answer requires the exact project, Lead and message ID.".into());
        }
        super::output_position(
            cursor.as_str().ok_or("Invalid output page cursor.")?,
            crate::MAX_ANSWER,
        )?;
    }
    if name == SEND_LEAD {
        if provider.is_none()
            || !args["projectKey"].is_string()
            || !args["leadRequestId"].as_str().is_some_and(uuid)
            || args.get("turnId").is_none()
            || !args["text"].as_str().is_some_and(text_ok)
        {
            return Err(
                "An exact project, Lead, message ID and 1–4000 bytes of text are required.".into(),
            );
        }
        crate::output::outbound(args["text"].as_str().unwrap().as_bytes(), &Value::Null)
            .map_err(|_| "OMS withheld this message during its outbound scan.")?;
    }
    Ok(())
}

pub(super) struct Conversation<'a> {
    repo: &'a Path,
    thread: &'a str,
    scope: &'a str,
    lead: &'a Value,
    provider: &'a str,
    dir: Directory,
}

impl<'a> Conversation<'a> {
    pub fn open(
        repo: &'a Path,
        thread: &'a str,
        scope: &'a str,
        lead: &'a Value,
        provider: &'a str,
    ) -> PanelResult<Self> {
        let dir = Directory::run(repo, thread, true).map_err(|_| INVALID)?;
        let this = Self {
            repo,
            thread,
            scope,
            lead,
            provider,
            dir,
        };
        this.bound()?;
        Ok(this)
    }
    fn bound(&self) -> PanelResult<()> {
        let current = Directory::run(self.repo, self.thread, true).map_err(|_| INVALID)?;
        let pinned = fs::fstat(&self.dir.fd).map_err(|_| INVALID)?;
        let named = fs::fstat(&current.fd).map_err(|_| INVALID)?;
        if named.st_dev != pinned.st_dev
            || named.st_ino != pinned.st_ino
            || named.st_mode & 0o077 != 0
        {
            return Err(INVALID.into());
        }
        Ok(())
    }
    fn read(&self, name: &str) -> PanelResult<Option<Value>> {
        self.bound()?;
        let stat = || fs::statat(&self.dir.fd, name, AtFlags::SYMLINK_NOFOLLOW);
        let st = match stat() {
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Ok(s)
                if FileType::from_raw_mode(s.st_mode) == FileType::RegularFile
                    && s.st_mode & 0o077 == 0 =>
            {
                s
            }
            _ => return Err(INVALID.into()),
        };
        let bytes = oms_safe_fs::Root::open(self.repo)
            .and_then(|r| {
                r.read(
                    &format!("{RUN_ROOT}/{}/{name}", self.thread),
                    32768,
                    oms_safe_fs::Policy::manifest(),
                )
            })
            .map_err(|_| INVALID)?;
        if !stat().is_ok_and(|s| {
            s.st_dev == st.st_dev && s.st_ino == st.st_ino && s.st_mode == st.st_mode
        }) || oms_context::parse_json_bounded(&bytes, 32768).is_err()
        {
            return Err(INVALID.into());
        }
        self.bound()?;
        let value = crate::value::from_slice(&bytes).map_err(|_| INVALID)?;
        Ok(Some(value))
    }
    fn head(&self) -> PanelResult<Option<String>> {
        self.read(HEAD)?
            .map(|v| {
                if v["scope"] != self.scope {
                    return Err(INVALID.to_owned());
                }
                v["id"]
                    .as_str()
                    .filter(|s| turn_id(s))
                    .map(str::to_owned)
                    .ok_or(INVALID.to_owned())
            })
            .transpose()
    }
    fn binding(&self, id: &str, digest: &str, route: &Value) -> String {
        if !route["model"].is_null() {
            return hash(
                json!([
                    "lead-message-route-v1",
                    self.scope,
                    self.lead["request"],
                    id,
                    digest,
                    route["model"],
                    route["effort"]
                ])
                .to_string(),
            );
        }
        hash(
            json!([
                "lead-message-v1",
                self.scope,
                self.lead["request"],
                id,
                digest
            ])
            .to_string(),
        )
    }
    fn record(&self, id: &str) -> PanelResult<Option<Value>> {
        if !turn_id(id) {
            return Err(INVALID.into());
        }
        let Some(v) = self.read(&format!("lead-turn-{id}.json"))? else {
            return Ok(None);
        };
        let text = v["text"].as_str().filter(|s| text_ok(s)).ok_or(INVALID)?;
        let digest = hash(text);
        if v["id"] != id
            || v["scope"] != self.scope
            || v["digest"] != digest
            || v.get("route")
                .is_some_and(|r| settings::selection(self.provider, r).is_err())
            || v["binding"] != self.binding(id, &digest, &v["route"])
            || !v["at"].is_string()
            || !(v["previous"].is_null()
                || v["previous"]
                    .as_str()
                    .is_some_and(|p| turn_id(p) && p != id))
        {
            return Err(INVALID.into());
        }
        Ok(Some(v))
    }
    fn absent(&self, op: &str) -> PanelResult<bool> {
        self.bound()?;
        let parent = Directory::open(self.repo, RUN_ROOT, false, true).map_err(|_| INVALID)?;
        match fs::statat(&parent.fd, op, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => Ok(true),
            Ok(_) => Ok(false),
            Err(_) => Err(INVALID.into()),
        }
    }
    fn completed_fingerprint(&self, op: &str) -> Option<String> {
        let stamp = |s: &rustix::fs::Stat| {
            json!([
                s.st_dev,
                s.st_ino,
                s.st_mode,
                s.st_size,
                s.st_mtime,
                s.st_mtime_nsec
            ])
        };
        let op_dir = Directory::run(self.repo, op, true).ok()?;
        let dir_stat = fs::fstat(&op_dir.fd)
            .ok()
            .filter(|s| s.st_mode & 0o077 == 0)?;
        let status_stat = fs::statat(&op_dir.fd, "status", AtFlags::SYMLINK_NOFOLLOW)
            .ok()
            .filter(|s| FileType::from_raw_mode(s.st_mode) == FileType::RegularFile)?;
        let meta_stat = fs::statat(&op_dir.fd, "meta.json", AtFlags::SYMLINK_NOFOLLOW)
            .ok()
            .filter(|s| FileType::from_raw_mode(s.st_mode) == FileType::RegularFile)?;
        if crate::peer::status(&op_dir.path) != ("done", Some(0)) {
            return None;
        }
        let answers =
            match Directory::open(self.repo, &format!("{RUN_ROOT}/{op}/answers"), false, true) {
                Ok(dir) => {
                    let dstat = fs::fstat(&dir.fd).ok()?;
                    let mut entries = Vec::new();
                    for entry in std::fs::read_dir(&dir.path).ok()? {
                        let entry = entry.ok()?;
                        let name = entry.file_name();
                        let st = fs::statat(&dir.fd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
                            .ok()?;
                        entries.push((name, stamp(&st)));
                    }
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    let files: Vec<Value> = entries
                        .into_iter()
                        .map(|(n, st)| json!([n.to_string_lossy().into_owned(), st]))
                        .collect();
                    json!([stamp(&dstat), files])
                }
                Err(_) => Value::Null,
            };
        Some(hash(
            json!([
                op,
                stamp(&dir_stat),
                stamp(&status_stat),
                stamp(&meta_stat),
                answers
            ])
            .to_string(),
        ))
    }
    fn observe_classified(
        &self,
        server: &Server<'_>,
        record: &Value,
    ) -> Result<(&'static str, Option<Value>), (TurnLookupError, String)> {
        let invalid = || (TurnLookupError::InvalidRecord, INVALID.to_owned());
        let binding = record["binding"].as_str().ok_or_else(invalid)?;
        let op = assignment_address(self.lead, binding).ok_or_else(invalid)?;
        let rejected = self
            .read(&format!(
                "lead-refused-{}.json",
                record["id"].as_str().ok_or_else(invalid)?
            ))
            .map_err(|_| invalid())?;
        let expected = json!({"id":record["id"],"binding":binding,"phase":"before-operation"});
        if rejected.as_ref().is_some_and(|v| v != &expected) {
            return Err(invalid());
        }
        let Some(dir) = checked_peer_directory(
            self.repo,
            self.lead,
            &op,
            self.thread,
            binding,
            true,
            Some((
                record["digest"].as_str().ok_or_else(invalid)?,
                &record["route"],
            )),
        )
        .map_err(|_| invalid())?
        else {
            let absent = self.absent(&op).map_err(|_| invalid())?;
            let status = match (absent, rejected.is_some()) {
                (true, true) => "failed",
                (true, false) => "reserved",
                _ => "unconfirmed",
            };
            return Ok((status, None));
        };
        if rejected.is_some() {
            return Ok(("unconfirmed", None));
        }
        let (state, exit) = crate::peer::status(&dir);
        match (state, exit) {
            ("running", _) => Ok(("running", None)),
            ("done", Some(0)) => {
                let proof = server
                    .peer_result(&json!({"repo":self.repo,"operation":op}))
                    .ok()
                    .and_then(|r| r.as_str().and_then(|s| crate::value::from_str(s).ok()));
                let answer = proof
                    .as_ref()
                    .filter(|v| v["operation"] == op && v["exit"] == 0)
                    .and_then(|v| v["answer_details"].as_array())
                    .and_then(|d| match d.as_slice() {
                        [detail]
                            if detail["exit"] == "0"
                                && detail["read_arguments"]["operation"] == op =>
                        {
                            detail["read_arguments"]["answer_ref"].as_str()
                        }
                        _ => None,
                    })
                    .and_then(|reference| {
                        let (name, _) = reference.rsplit_once(':')?;
                        let (text, exit) =
                            crate::storage::owned_answer(self.repo, &op, name).ok()?;
                        (exit == "0"
                            && reference == crate::storage::reference(&op, name, &text)
                            && !text.trim().is_empty())
                        .then_some(text)
                    });
                if let Some(answer) = answer {
                    oms_scrub::outbound_page(
                        &answer,
                        crate::MAX_ANSWER,
                        oms_scrub::PageSelector::Offset(0),
                    )
                    .map_err(|_| {
                        (
                            TurnLookupError::OutboundWithheld,
                            "OMS withheld this Lead answer during its outbound scan.".to_owned(),
                        )
                    })?;
                    Ok((
                        "replied",
                        Some(
                            json!({"role":self.lead["provider"],"text":byte_prefix(&answer,4000),"turnId":record["id"],"at":Value::Null,"truncated":answer.len()>4000}),
                        ),
                    ))
                } else {
                    Ok(("unconfirmed", None))
                }
            }
            ("done", Some(_)) => Ok(("failed", None)),
            _ => Ok(("unconfirmed", None)),
        }
    }
    fn observe(
        &self,
        server: &Server<'_>,
        record: &Value,
    ) -> PanelResult<(&'static str, Option<Value>)> {
        self.observe_classified(server, record)
            .map_err(|(_, msg)| msg)
    }
    fn submit(
        &self,
        server: &Server<'_>,
        args: &Value,
        check: &impl Fn() -> PanelResult<()>,
    ) -> PanelResult<()> {
        let id = args["turnId"].as_str().ok_or(INVALID)?;
        let text = args["text"].as_str().ok_or(INVALID)?;
        // The kickoff has already exited. Its same private inode lock serializes
        // all panel writers across app processes; reads don't create/take a lock.
        let _guard = self.dir.lock().map_err(
            |_| "Another Lead message is being checked. Read the original request status.",
        )?;
        check()?;
        if let Some(prior) = self.record(id)? {
            if prior["text"] != text {
                return Err("That message ID already belongs to different text.".into());
            }
            return Ok(());
        }
        let previous = self.head()?;
        if let Some(head) = &previous {
            let last = self.record(head)?.ok_or(INVALID)?;
            if !matches!(self.observe(server, &last)?.0, "replied" | "failed") {
                return Err("The previous message still has an unresolved delivery. Check its status first.".into());
            }
        }
        let digest = hash(text);
        let route = self.current_settings()?["selection"].clone();
        let settings = settings::selection(self.provider, &route)?;
        if settings.is_some() {
            self.check_selection(server, &route)?;
        }
        let binding = self.binding(id, &digest, &route);
        let op = assignment_address(self.lead, &binding).ok_or(INVALID)?;
        let record = json!({"id":id,"route":route,"scope":self.scope,"binding":binding,"digest":digest,"text":text,"previous":previous,"at":crate::storage::now()});
        self.bound()?;
        self.dir
            .publish_new(
                &format!("lead-turn-{id}.json"),
                record.to_string().as_bytes(),
            )
            .map_err(|_| INVALID)?;
        if self.record(id)?.as_ref() != Some(&record) {
            return Err(INVALID.into());
        }
        self.dir
            .atomic(
                HEAD,
                json!({"scope":self.scope,"id":id}).to_string().as_bytes(),
            )
            .map_err(|_| INVALID)?;
        check()?;
        if self.head()?.as_deref() != Some(id) {
            return Err(INVALID.into());
        }
        // Once reserved, an uncertain return can only be observed. Reusing this
        // UUID must never dispatch again, including across a crash/response loss.
        let initially_absent = self.absent(&op)?;
        let launch = server.start_lead_followup(
            self.repo,
            self.provider,
            text,
            &op,
            self.thread,
            &binding,
            settings,
        );
        // A physical launch requires mkdir of this operation before spawn; that
        // helper never removes it. Only the original writer may attest a synchronous
        // refusal before that boundary. Absence on any later read is inconclusive.
        if launch.is_err() && initially_absent && self.absent(&op)? {
            check()?;
            self.dir
                .publish_new(
                    &format!("lead-refused-{id}.json"),
                    json!({"id":id,"binding":binding,"phase":"before-operation"})
                        .to_string()
                        .as_bytes(),
                )
                .map_err(|_| INVALID)?;
        }
        Ok(())
    }
    pub fn latest(
        &self,
        server: &Server<'_>,
        force_refresh: bool,
        check: impl Fn() -> PanelResult<()>,
    ) -> Result<Value, TurnLookupError> {
        let Some(id) = self.head().map_err(|_| TurnLookupError::InvalidRecord)? else {
            return Ok(Value::Null);
        };
        let record = self
            .record(&id)
            .map_err(|_| TurnLookupError::InvalidRecord)?
            .ok_or(TurnLookupError::InvalidRecord)?;
        let binding = record["binding"]
            .as_str()
            .ok_or(TurnLookupError::InvalidRecord)?;
        let op = assignment_address(self.lead, binding).ok_or(TurnLookupError::InvalidRecord)?;
        let cache_key = (self.scope.to_owned(), id.clone());
        let cached = if !force_refresh {
            let rejected = self
                .read(&format!("lead-refused-{id}.json"))
                .map_err(|_| TurnLookupError::InvalidRecord)?;
            if rejected.is_none()
                && let Some(fp) = self.completed_fingerprint(&op)
                && let Ok(guard) = server.lead_turns.0.lock()
                && let Some((saved_fp, outcome)) = guard.get(&cache_key)
                && saved_fp == &fp
            {
                Some(*outcome)
            } else {
                None
            }
        } else {
            None
        };
        let status = match cached {
            Some(outcome) => outcome?,
            None => {
                let outcome = self
                    .observe_classified(server, &record)
                    .map(|(s, _)| s)
                    .map_err(|(e, _)| e);
                if let Some(fp) = self.completed_fingerprint(&op)
                    && let Ok(mut guard) = server.lead_turns.0.lock()
                {
                    if guard.len() >= 64 && !guard.contains_key(&cache_key) {
                        guard.clear();
                    }
                    guard.insert(cache_key, (fp, outcome));
                }
                outcome?
            }
        };
        self.bound().map_err(|_| TurnLookupError::ReceiptChanged)?;
        check().map_err(|_| TurnLookupError::ReceiptChanged)?;
        if self
            .head()
            .map_err(|_| TurnLookupError::ReceiptChanged)?
            .as_deref()
            != Some(id.as_str())
            || self
                .record(&id)
                .map_err(|_| TurnLookupError::ReceiptChanged)?
                .as_ref()
                != Some(&record)
        {
            return Err(TurnLookupError::ReceiptChanged);
        }
        Ok(
            json!({"turnId":id,"status":status,"submittedAt":record["at"],"lastObservedAt":crate::storage::now()}),
        )
    }

    pub fn answer_page(
        &self,
        server: &Server<'_>,
        args: &Value,
        check: impl Fn() -> PanelResult<()>,
    ) -> PanelResult<Value> {
        let id = args["turnId"].as_str().ok_or(INVALID)?;
        let record = self
            .record(id)?
            .ok_or("The selected Lead message is unavailable.")?;
        if self.observe(server, &record)?.0 != "replied" {
            return Err("The selected Lead message has no verified completed answer.".into());
        }
        let binding = record["binding"].as_str().ok_or(INVALID)?;
        let operation = assignment_address(self.lead, binding).ok_or(INVALID)?;
        let result = server
            .peer_result(&json!({"repo":self.repo,"operation":operation}))
            .map_err(|_| INVALID)?;
        let result =
            crate::value::from_str(result.as_str().ok_or(INVALID)?).map_err(|_| INVALID)?;
        let details = result["answer_details"].as_array().ok_or(INVALID)?;
        let [detail] = details.as_slice() else {
            return Err(INVALID.into());
        };
        if result["operation"] != operation
            || result["exit"] != 0
            || detail["exit"] != "0"
            || detail["read_arguments"]["operation"] != operation
        {
            return Err(INVALID.into());
        }
        let reference = detail["read_arguments"]["answer_ref"]
            .as_str()
            .ok_or(INVALID)?;
        let (name, _) = reference.rsplit_once(':').ok_or(INVALID)?;
        if !crate::answer_name(name) {
            return Err(INVALID.into());
        }
        let operation_dir = Directory::run(self.repo, &operation, true).map_err(|_| INVALID)?;
        let directory_identity = |dir: &Directory| {
            fs::fstat(&dir.fd)
                .map(|s| (s.st_dev, s.st_ino, s.st_mode))
                .map_err(|_| INVALID)
        };
        let operation_identity = directory_identity(&operation_dir)?;
        let answers = Directory::open(
            self.repo,
            &format!("{RUN_ROOT}/{operation}/answers"),
            false,
            true,
        )
        .map_err(|_| INVALID)?;
        let stat = || fs::statat(&answers.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| INVALID);
        let before = stat()?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile
            || before.st_mode & 0o077 != 0
        {
            return Err(INVALID.into());
        }
        let identity = |s: &rustix::fs::Stat| (s.st_dev, s.st_ino, s.st_mode, s.st_size);
        let revision = hash(
            json!([
                self.scope,
                binding,
                reference,
                before.st_dev,
                before.st_ino,
                before.st_mode,
                before.st_size,
                [
                    operation_identity.0,
                    operation_identity.1,
                    operation_identity.2
                ]
            ])
            .to_string(),
        );
        let cursor = args["answerPage"].as_str().ok_or(INVALID)?;
        let offset = super::output_position(cursor, crate::MAX_ANSWER)?;
        super::check_output_cursor(cursor, self.scope, &revision, offset)?;
        if crate::peer::status(&operation_dir.path) != ("done", Some(0))
            || crate::storage::meta(&operation_dir.path)["answer_storage"] != "operation"
        {
            return Err("The retained answer changed. Reopen the result.".into());
        }
        let (answer, artifact_exit) = crate::storage::owned_answer(self.repo, &operation, name)
            .map_err(|_| "The retained answer changed. Reopen the result.")?;
        let digest = crate::storage::hash(&answer);
        if artifact_exit != "0"
            || reference != crate::storage::reference(&operation, name, &answer)
            || detail["answer_sha256"].as_str() != Some(digest.as_str())
            || identity(&stat()?) != identity(&before)
        {
            return Err("The retained answer changed. Reopen the result.".into());
        }
        let page = oms_scrub::outbound_page(
            &answer,
            crate::MAX_ANSWER,
            oms_scrub::PageSelector::Offset(offset),
        )
        .map_err(|err| match err {
            oms_scrub::PageError::OutboundWithheld => {
                "OMS withheld this Lead answer during its outbound scan."
            }
            oms_scrub::PageError::InvalidRange => INVALID,
        })?;
        self.bound()?;
        check()?;
        let checked = checked_peer_directory(
            self.repo,
            self.lead,
            &operation,
            self.thread,
            binding,
            true,
            Some((record["digest"].as_str().ok_or(INVALID)?, &record["route"])),
        )?
        .ok_or(INVALID)?;
        if self.record(id)?.as_ref() != Some(&record)
            || operation_identity
                != directory_identity(
                    &Directory::run(self.repo, &operation, true).map_err(|_| INVALID)?,
                )?
            || directory_identity(&answers)?
                != directory_identity(
                    &Directory::open(
                        self.repo,
                        &format!("{RUN_ROOT}/{operation}/answers"),
                        false,
                        true,
                    )
                    .map_err(|_| INVALID)?,
                )?
            || crate::peer::status(&checked) != ("done", Some(0))
        {
            return Err(INVALID.into());
        }
        Ok(
            json!({"turnId":id,"text":page.text,"offset":page.offset,"bytes":page.text.len(),"totalBytes":page.total_bytes,
            "digest":digest,"nextCursor":page.has_more.then(||super::output_cursor(self.scope,&revision,page.end)),
            "hasMore":page.has_more,"source":"recorded-answer",
            "handoff":{"room":self.lead["room"],"participant":self.lead["participant"],"provider":self.provider,
                "leadRequestId":self.lead["request"],"turnId":id,"operation":operation}}),
        )
    }
    pub fn exchange(
        &self,
        server: &Server<'_>,
        name: &str,
        args: &Value,
        transcript: &mut Value,
        check: impl Fn() -> PanelResult<()>,
    ) -> PanelResult<Option<Value>> {
        if name == SEND_LEAD {
            self.submit(server, args, &check)?;
        }
        let selected = args["turnId"]
            .as_str()
            .map(|id| -> PanelResult<Value> {
                let status = match self.record(id)? {
                    Some(r) => self.observe(server, &r)?.0,
                    None => "not_found",
                };
                Ok(json!({"turnId":id,"status":status}))
            })
            .transpose()?;
        let mut cursor = match args["before"].as_str() {
            Some(id) => {
                if self.record(id)?.is_none() {
                    return Err("Invalid Lead conversation page.".into());
                }
                Some(id.to_owned())
            }
            None => self.head()?,
        };
        let mut page = Vec::new();
        let mut bytes = 0;
        for _ in 0..PAGE {
            let Some(id) = &cursor else { break };
            let r = self.record(id)?.ok_or(INVALID)?;
            let (status, answer) = self.observe(server, &r)?;
            let user =
                json!({"role":"user","text":r["text"],"turnId":id,"at":r["at"],"status":status});
            let size = user.to_string().len() + answer.as_ref().map_or(0, |v| v.to_string().len());
            if bytes + size > 28_000 && !page.is_empty() {
                break;
            }
            if let Some(answer) = answer {
                page.push(answer);
            }
            page.push(user);
            bytes += size;
            cursor = r["previous"].as_str().map(str::to_owned);
        }
        page.reverse();
        if cursor.is_none() {
            let mut all = transcript["turns"].as_array().cloned().unwrap_or_default();
            all.extend(page);
            page = all;
        }
        *transcript = json!({"turns":page,"cursor":cursor,"hasMore":cursor.is_some()});
        self.bound()?;
        check()?;
        Ok(selected)
    }
}
