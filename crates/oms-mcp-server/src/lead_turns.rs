//! User messages in an existing Room-bound OMS Lead conversation. The original
//! private operation owns the index. Each turn has one immutable reservation
//! and a distinct peer operation, even when the same text is sent twice.
use super::{PanelResult, SEND_LEAD, assignment_address, checked_peer_directory, uuid};
use crate::storage::{Directory, byte_prefix, hash};
use crate::value::Value;
use crate::{RUN_ROOT, Server};
use rustix::fs::{self, AtFlags, FileType};
use std::path::Path;

#[path = "lead_settings.rs"]
mod settings;

const INVALID: &str = "The Lead conversation receipt is unavailable or changed.";
const HEAD: &str = "lead-turn-head.json";
const PAGE: usize = 4;

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
    fn observe(
        &self,
        server: &Server<'_>,
        record: &Value,
    ) -> PanelResult<(&'static str, Option<Value>)> {
        let binding = record["binding"].as_str().ok_or(INVALID)?;
        let op = assignment_address(self.lead, binding).ok_or(INVALID)?;
        let rejected = self.read(&format!(
            "lead-refused-{}.json",
            record["id"].as_str().ok_or(INVALID)?
        ))?;
        let expected = json!({"id":record["id"],"binding":binding,"phase":"before-operation"});
        if rejected.as_ref().is_some_and(|v| v != &expected) {
            return Err(INVALID.into());
        }
        let Some(dir) = checked_peer_directory(
            self.repo,
            self.lead,
            &op,
            self.thread,
            binding,
            true,
            Some((record["digest"].as_str().ok_or(INVALID)?, &record["route"])),
        )?
        else {
            let absent = self.absent(&op)?;
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
                    .filter(|v| {
                        v["operation"] == op
                            && v["exit"] == 0
                            && v["answer_details"].as_array().is_some_and(|d| {
                                d.len() == 1
                                    && d[0]["exit"] == "0"
                                    && d[0]["read_arguments"]["operation"] == op
                            })
                    })
                    .and_then(|v| v["answer"].as_str())
                    .filter(|s| !s.trim().is_empty());
                if let Some(answer) = answer {
                    crate::output::outbound(answer.as_bytes(), &Value::Null)
                        .map_err(|_| "OMS withheld this Lead answer during its outbound scan.")?;
                    Ok((
                        "replied",
                        Some(
                            json!({"role":self.lead["provider"],"text":byte_prefix(answer,4000),"turnId":record["id"],"at":Value::Null,"truncated":answer.len()>4000 || proof.as_ref().is_some_and(|v|v["answer_truncated"]==true)}),
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
