//! Optional provider-reported identity for one successfully published, protected
//! panel Lead answer. None of this data selects an account or resumes a session.
use crate::{Context, V, obj, storage};
use oms_json::Value;
use oms_safe_fs::{ByteReadOptions, MutationOptions, Root};
use std::path::{Path, PathBuf};

pub(crate) struct Observation {
    session: String,
    model: Option<String>,
}

fn string(row: &Value, key: &str) -> Option<String> {
    row.get(key)?.as_string()?.to_str_checked()
}

fn session(value: String) -> Option<String> {
    cached_regex!(r"\A[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\z")
        .is_match(&value)
        .then_some(value)
}

pub(crate) fn observe(provider: &str, raw: &[u8]) -> Option<Observation> {
    // The provider capture's own cap; partial-message rows add ~450 bytes per delta.
    if raw.len() > 64 * 1024 * 1024 {
        return None;
    }
    if provider == "antigravity" {
        let row = oms_json::parse_strict(raw).ok()?;
        if string(&row, "status").as_deref() != Some("SUCCESS")
            || string(&row, "response")?.trim().is_empty()
            || row.get("error").is_some_and(|v| !matches!(v, Value::Null))
            || crate::providers::native_stopped_early(&row)
        {
            return None;
        }
        return Some(Observation {
            session: session(string(&row, "conversation_id")?)?,
            model: None,
        });
    }
    if provider != "claude" {
        return None;
    }
    let mut id = None;
    let mut model = None;
    let mut model_unknown = false;
    let mut complete = false;
    let model_id = cached_regex!(r"\Aclaude-[A-Za-z0-9][A-Za-z0-9._-]{0,95}\z");
    let mut rows = 0;
    for line in std::str::from_utf8(raw)
        .ok()?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let row = oms_json::parse_strict(line.as_bytes()).ok()?;
        if crate::providers::native_stopped_early(&row) {
            return None;
        }
        let kind = string(&row, "type")?;
        // Partial-message deltas are bounded by the byte cap, not the row cap.
        if kind != "stream_event" {
            rows += 1;
            if rows > 2048 {
                return None;
            }
        }
        if complete {
            if row.get("session_id").is_some()
                && string(&row, "session_id")?.as_str() != id.as_deref()?
            {
                return None;
            }
            let footer = V::parse(line.as_bytes()).ok()?;
            if !crate::providers::native_result_footer(provider, &kind, &footer) {
                return None;
            }
            continue;
        }
        match kind.as_str() {
            "system" if string(&row, "subtype").as_deref() == Some("init") => {
                if id.is_some() {
                    return None;
                }
                id = Some(session(string(&row, "session_id")?)?);
            }
            "assistant" if matches!(row.get("parent_tool_use_id"), Some(Value::Null)) => {
                if string(&row, "session_id")?.as_str() != id.as_deref()? {
                    return None;
                }
                let m = row
                    .get("message")
                    .and_then(|v| string(v, "model"))
                    .filter(|m| model_id.is_match(m) && !m.contains(".."));
                if m.is_none() || model.is_some() && model != m {
                    model_unknown = true;
                }
                model = m;
            }
            "result" => {
                if string(&row, "session_id")?.as_str() != id.as_deref()?
                    || string(&row, "subtype").as_deref() != Some("success")
                    || !matches!(row.get("is_error"), Some(Value::Bool(false)))
                    || string(&row, "result")?.trim().is_empty()
                {
                    return None;
                }
                complete = true;
            }
            "error" => return None,
            _ => {}
        }
    }
    if !complete {
        return None;
    }
    Some(Observation {
        session: id?,
        model: model.filter(|m| !model_unknown && crate::safety::outbound(m.as_bytes()).is_ok()),
    })
}

/// True only when this protected Lead owner exported the session its own
/// launch and meta receipts name; an ambient selector never qualifies.
pub(crate) fn resumed(
    repo: &Path,
    answers: &Path,
    thread: &str,
    provider: &str,
    context: &Context<'_>,
) -> bool {
    let Some(session) = std::env::var("OMS_RESUME_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    Scope::from_owner(repo, answers, thread, provider, context).is_some_and(|scope| {
        [&scope.meta, &scope.launch].into_iter().all(|raw| {
            oms_json::parse_strict(raw)
                .ok()
                .and_then(|row| string(&row, "lead_resume"))
                .is_some_and(|id| id == session)
        })
    })
}

pub(crate) struct Scope {
    run: Root,
    operation: String,
    provider: String,
    // Recheck the same owner's exact private generation after the provider returns.
    meta: Vec<u8>,
    launch: Vec<u8>,
}

impl Scope {
    pub(crate) fn from_owner(
        repo: &Path,
        answers: &Path,
        thread: &str,
        provider: &str,
        context: &Context<'_>,
    ) -> Option<Self> {
        let op = std::env::var("OMS_PEER_LEAD_OBSERVATION").ok()?;
        let run_path = Path::new(oms_command_operations::peer_operations::RUN_ROOT).join(&op);
        if !cached_regex!(r"\Aask-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{64}\z").is_match(&op)
            || answers != run_path.join("answers")
            || std::env::var_os("OMS_PEER_CANCEL_DIR")? != repo.join(&run_path).as_os_str()
        {
            return None;
        }
        let resolver = context.executor.group_resolver();
        let uid = rustix::process::getuid().as_raw();
        let run = Root::open_private_workspace(repo.join(run_path), |gid| {
            resolver.and_then(|r| r(gid).ok()).is_some_and(|proof| {
                proof.gid == gid
                    && proof.primary_member_uids == [uid]
                    && proof.explicit_member_uids.iter().all(|u| *u == uid)
            })
        })
        .ok()?;
        Self::checked(run, op, thread, provider)
    }

    fn private(run: &Root, key: &str) -> Option<Vec<u8>> {
        run.read_no_follow(
            Path::new(key),
            ByteReadOptions {
                missing_ok: false,
                max_bytes: 256 * 1024,
            },
        )
        .ok()
    }

    fn checked(run: Root, operation: String, thread: &str, provider: &str) -> Option<Self> {
        let meta = Self::private(&run, "meta.json")?;
        let launch = Self::private(&run, "launch.json")?;
        let m = oms_json::parse_strict(&meta).ok()?;
        let l = oms_json::parse_strict(&launch).ok()?;
        let binding = string(&m, "panel_assignment")?;
        if !cached_regex!(r"\A[0-9a-f]{64}\z").is_match(&binding)
            || !operation.ends_with(&storage::hash(
                format!("assignment-operation/{binding}").as_bytes(),
            ))
            || string(&m, "operation")? != operation
            || string(&m, "answer_storage")? != "operation"
            || !matches!(l.get("preserve_current"), Some(Value::Bool(true)))
        {
            return None;
        }
        if thread != operation
            && (!matches!(m.get("lead_followup"), Some(Value::Bool(true)))
                || string(&l, "lead_followup")? != binding)
        {
            return None;
        }
        for row in [&m, &l] {
            if string(row, "thread")? != thread || string(row, "kind")? != "ask" {
                return None;
            }
            let [target] = row.get("targets")?.as_array()? else {
                return None;
            };
            if target.as_string()?.to_str_checked()? != provider {
                return None;
            }
        }
        Some(Self {
            run,
            operation,
            provider: provider.into(),
            meta,
            launch,
        })
    }

    pub(crate) fn publish(self, observation: &Observation, artifact: &Path, hash: &str) {
        if Self::private(&self.run, "meta.json") != Some(self.meta)
            || Self::private(&self.run, "launch.json") != Some(self.launch)
        {
            return;
        }
        let Some(name) = artifact.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        if !name.ends_with(".md") || !storage::identifier(name) {
            return;
        }
        let record = obj([
            ("schema", 1usize.into()),
            ("operation", self.operation.into()),
            ("provider", self.provider.into()),
            ("sessionId", observation.session.clone().into()),
            ("model", observation.model.clone().map_or(V::Null, V::from)),
            ("artifact", name.into()),
            ("artifactSha256", hash.into()),
        ]);
        // Missing optional evidence must never cause a paid call to be repeated.
        if let Ok(bytes) = record.json(false) {
            let _ = self.run.create_private(
                Path::new("answers/native-lead-observation.json"),
                &bytes,
                2048,
            );
        }
    }
}

/// Display-only partial reply of one protected Claude Lead call: an append-only
/// JSONL log the panel reads by byte cursor. Each text line passed the shared
/// outbound scan together with everything published before it; after a
/// withheld, oversized or failed write nothing more is published.
pub(crate) struct Stream(std::sync::Mutex<Tail>);

pub(crate) const STREAM_LOG: &str = "lead-stream.jsonl";
const STREAM_MAX: usize = 128 * 1024;

struct Tail {
    run: Root,
    cwd: PathBuf,
    seen: usize,
    pending: String,
    text: String,
    log: Vec<u8>,
    written: usize,
    blocks: std::collections::BTreeSet<u64>,
    closed: bool,
    flushed: Option<std::time::Instant>,
}

impl Stream {
    pub(crate) fn from_owner(
        repo: &Path,
        answers: &Path,
        thread: &str,
        provider: &str,
        cwd: &Path,
        context: &Context<'_>,
    ) -> Option<Self> {
        if provider != "claude" {
            return None;
        }
        let scope = Scope::from_owner(repo, answers, thread, provider, context)?;
        Some(Self(std::sync::Mutex::new(Tail {
            run: scope.run,
            cwd: cwd.to_owned(),
            seen: 0,
            pending: String::new(),
            text: String::new(),
            log: Vec::new(),
            written: 0,
            blocks: Default::default(),
            closed: false,
            flushed: None,
        })))
    }

    /// `stdout` is the provider's whole stdout prefix so far.
    pub(crate) fn observe(&self, stdout: &[u8]) {
        if let Ok(mut tail) = self.0.lock() {
            tail.feed(stdout);
        }
    }
}

impl Tail {
    fn feed(&mut self, raw: &[u8]) {
        // A shorter prefix is a retried call; its output is not this reply's.
        if raw.len() < self.seen {
            self.closed = true;
        }
        if self.closed {
            return;
        }
        let Some(end) = raw[self.seen..].iter().rposition(|b| *b == b'\n') else {
            return;
        };
        let fresh = &raw[self.seen..self.seen + end + 1];
        self.seen += end + 1;
        for line in fresh.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            if let Ok(row) = oms_json::parse_strict(line) {
                self.event(&row);
            }
        }
        if let Some(cut) = self.pending.rfind('\n') {
            let chunk: String = self.pending.drain(..=cut).collect();
            let mut scanned = self.text.clone();
            scanned.push_str(&chunk);
            if crate::safety::outbound(scanned.as_bytes()).is_ok() {
                self.text = scanned;
                self.append("text", &chunk);
            } else {
                self.append("withheld", "");
                self.closed = true;
            }
        }
        if self.written < self.log.len()
            && self
                .flushed
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(250))
        {
            self.flush();
        }
    }

    fn event(&mut self, row: &Value) {
        // Subagent output belongs to a tool call, not this reply.
        if !matches!(row.get("parent_tool_use_id"), Some(Value::Null)) {
            return;
        }
        let index = |e: &Value| {
            e.get("index")
                .and_then(Value::as_number_f64)
                .map(|n| n as u64)
        };
        match string(row, "type").as_deref() {
            Some("stream_event") => {
                let Some(event) = row.get("event") else {
                    return;
                };
                match string(event, "type").as_deref() {
                    Some("message_start") => self.blocks.clear(),
                    Some("content_block_start")
                        if event
                            .get("content_block")
                            .and_then(|b| string(b, "type"))
                            .as_deref()
                            == Some("text") =>
                    {
                        if let Some(i) = index(event) {
                            self.blocks.insert(i);
                        }
                        if !self.text.is_empty() || !self.pending.is_empty() {
                            self.pending.push('\n');
                        }
                    }
                    Some("content_block_delta") => {
                        if let Some(delta) = event.get("delta")
                            && string(delta, "type").as_deref() == Some("text_delta")
                            && index(event).is_some_and(|i| self.blocks.contains(&i))
                            && let Some(text) = string(delta, "text")
                        {
                            self.pending.push_str(&text);
                        }
                    }
                    Some("content_block_stop") => {
                        if index(event).is_some_and(|i| self.blocks.remove(&i))
                            && !self.pending.is_empty()
                            && !self.pending.ends_with('\n')
                        {
                            self.pending.push('\n');
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                for part in row
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array)
                    .unwrap_or_default()
                {
                    if string(part, "type").as_deref() == Some("tool_use")
                        && let Some(line) = self.activity(part)
                    {
                        self.append("tool", &line);
                    }
                }
            }
            _ => {}
        }
    }

    /// One short "Tool detail" line; the detail is dropped when it is not
    /// publishable, and a repository path is shown relative to the checkout.
    fn activity(&self, part: &Value) -> Option<String> {
        let name = string(part, "name")
            .filter(|n| cached_regex!(r"\A[A-Za-z][A-Za-z0-9_.:-]{0,63}\z").is_match(n))?;
        let input = part.get("input");
        let detail = ["file_path", "path", "pattern", "command", "query", "url"]
            .into_iter()
            .find_map(|key| input.and_then(|i| string(i, key)))
            .map(|d| {
                let d = d.lines().next().unwrap_or_default();
                let d = Path::new(d)
                    .strip_prefix(&self.cwd)
                    .ok()
                    .and_then(Path::to_str)
                    .unwrap_or(d);
                d.chars().take(96).collect::<String>()
            })
            .filter(|d| !d.trim().is_empty());
        let line = match detail {
            Some(d) => format!("{name} {d}"),
            None => name.clone(),
        };
        if crate::safety::outbound(line.as_bytes()).is_ok() {
            Some(line)
        } else {
            crate::safety::outbound(name.as_bytes())
                .is_ok()
                .then_some(name)
        }
    }

    fn append(&mut self, kind: &str, text: &str) {
        let Ok(mut line) = obj([("kind", kind.into()), ("text", text.into())]).json(false) else {
            self.closed = true;
            return;
        };
        line.push(b'\n');
        if self.log.len() + line.len() > STREAM_MAX - 64 {
            self.log
                .extend_from_slice(b"{\"kind\":\"truncated\",\"text\":\"\"}\n");
            self.closed = true;
        } else {
            self.log.extend_from_slice(&line);
        }
        if self.closed {
            self.flush();
        }
    }

    fn flush(&mut self) {
        let log = self.log.clone();
        let written = self.run.atomic_mutate(
            Path::new(STREAM_LOG),
            MutationOptions {
                read: ByteReadOptions {
                    missing_ok: true,
                    max_bytes: STREAM_MAX,
                },
                max_output: STREAM_MAX,
                ..Default::default()
            },
            |_| Ok::<_, std::convert::Infallible>(log),
        );
        self.flushed = Some(std::time::Instant::now());
        if written.is_ok() {
            self.written = self.log.len();
        } else {
            // Optional display evidence never delays or fails the paid call.
            self.closed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_reply_publishes_scanned_lines_and_tool_activity_only() {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::env::temp_dir().join(format!("oms-lead-stream-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let cwd = Path::new("/work/tree");
        let tail = |rows: &[serde_json::Value]| {
            let mut tail = Tail {
                run: Root::open(&dir).unwrap(),
                cwd: cwd.into(),
                seen: 0,
                pending: String::new(),
                text: String::new(),
                log: Vec::new(),
                written: 0,
                blocks: Default::default(),
                closed: false,
                flushed: None,
            };
            let raw: String = rows.iter().map(|r| format!("{r}\n")).collect();
            // Polls see growing prefixes that may end inside a row.
            for end in (1..raw.len()).step_by(37).chain([raw.len()]) {
                tail.feed(&raw.as_bytes()[..end]);
            }
            tail.flush();
            std::fs::read_to_string(dir.join(STREAM_LOG))
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .map(|v| {
                    (
                        v["kind"].as_str().unwrap().to_owned(),
                        v["text"].as_str().unwrap().to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        // Shapes from `claude -p --output-format stream-json --verbose --include-partial-messages`.
        let event = |e: serde_json::Value| json!({"type":"stream_event","event":e,"parent_tool_use_id":null});
        let delta = |i: u64, text: &str| {
            event(
                json!({"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":text}}),
            )
        };
        let mut rows = vec![
            json!({"type":"system","subtype":"init","session_id":"s"}),
            event(json!({"type":"message_start","message":{}})),
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
            ),
            event(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private"}}),
            ),
            event(json!({"type":"content_block_stop","index":0})),
            json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[
                {"type":"tool_use","name":"Read","input":{"file_path":"/work/tree/AGENTS.md","limit":1}}]}}),
            json!({"type":"assistant","parent_tool_use_id":"child","message":{"content":[
                {"type":"tool_use","name":"Grep","input":{"pattern":"subagent"}}]}}),
            event(json!({"type":"message_start","message":{}})),
            event(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            delta(0, "The first line of AGENT"),
            delta(0, "S.md is a heading.\n\nPlan mode is still act"),
            delta(0, "ive."),
            event(json!({"type":"content_block_stop","index":0})),
        ];
        let mut child = delta(0, "subagent text\n");
        child["parent_tool_use_id"] = json!("child");
        rows.insert(10, child);
        let published = tail(&rows);
        assert_eq!(published[0], ("tool".into(), "Read AGENTS.md".into()));
        assert!(
            published[1..]
                .iter()
                .all(|(k, t)| k == "text" && t.ends_with('\n'))
        );
        let text: String = published[1..].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            text,
            "The first line of AGENTS.md is a heading.\n\nPlan mode is still active.\n"
        );
        let planted = ["gh", "p_0123456789abcdef0123456789abcdef0123"].concat();
        rows.insert(11, delta(0, &format!("token {planted}\n")));
        rows.push(event(
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text"}}),
        ));
        rows.push(delta(1, "after\n"));
        let withheld = tail(&rows);
        assert_eq!(withheld.last().unwrap().0, "withheld");
        assert!(
            withheld
                .iter()
                .all(|(_, t)| !t.contains("after") && !t.contains("p_0123"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn observed_identity_survives_partial_messages_and_post_result_summary() {
        let id = "12345678-1234-4234-8234-123456789abc";
        let mut raw = vec![json!({"type":"system","subtype":"init","session_id":id}).to_string()];
        let delta = json!({"type":"stream_event","session_id":id,"parent_tool_use_id":null,
            "event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}});
        raw.extend(std::iter::repeat_n(delta.to_string(), 3000));
        raw.push(json!({"type":"result","session_id":id,"subtype":"success","is_error":false,"result":"a"}).to_string());
        raw.push(
            json!({"type":"system","subtype":"task_summary","detail":null,"session_id":id})
                .to_string(),
        );
        assert_eq!(
            observe("claude", raw.join("\n").as_bytes())
                .unwrap()
                .session,
            id
        );
    }

    #[test]
    fn observed_identity_requires_one_native_success_and_one_served_root_model() {
        let id = "12345678-1234-4234-8234-123456789abc";
        let init = json!({"type":"system","subtype":"init","session_id":id,"model":"configured"});
        let assistant = json!({"type":"assistant","session_id":id,"parent_tool_use_id":null,"message":{"model":"claude-sonnet-5-5"}});
        let result = json!({"type":"result","session_id":id,"subtype":"success","is_error":false,"result":"answer"});
        let run = |rows: &[&serde_json::Value]| {
            observe(
                "claude",
                rows.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .as_bytes(),
            )
        };
        let observation = run(&[&init, &assistant, &result]).unwrap();
        assert_eq!(observation.session, id);
        assert_eq!(observation.model.as_deref(), Some("claude-sonnet-5-5"));
        assert!(run(&[&init, &result]).unwrap().model.is_none());
        let mut subagent = assistant.clone();
        subagent["parent_tool_use_id"] = json!("child");
        assert!(run(&[&init, &subagent, &result]).unwrap().model.is_none());
        let mut footer = json!({"type":"system","session_id":id,"subtype":"hook_response","hook_event":"Stop","outcome":"success"});
        assert!(run(&[&init, &assistant, &result, &footer]).is_some());
        footer["session_id"] = json!("12345678-1234-4234-8234-000000000001");
        assert!(run(&[&init, &assistant, &result, &footer]).is_none());
        assert!(run(&[&init, &assistant, &result, &result]).is_none());
        let mut other = assistant.clone();
        other["message"]["model"] = json!("claude-opus-5");
        assert!(
            run(&[&init, &assistant, &other, &result])
                .unwrap()
                .model
                .is_none()
        );
        let mut wrong = result.clone();
        wrong["session_id"] = json!("12345678-1234-4234-8234-000000000001");
        assert!(run(&[&init, &assistant, &wrong]).is_none());
        assert!(run(&[&init, &init, &result]).is_none());
        assert!(run(&[&init, &result, &assistant]).is_none());
        assert!(run(&[&assistant, &result]).is_none());
        let mut agy = json!({"status":"SUCCESS","conversation_id":id,"response":"answer","model":"requested"});
        assert!(
            observe("antigravity", agy.to_string().as_bytes())
                .unwrap()
                .model
                .is_none()
        );
        for bad in ["opaque", "00000000-0000-0000-0000-000000000000"] {
            agy["conversation_id"] = json!(bad);
            assert!(observe("antigravity", agy.to_string().as_bytes()).is_none());
        }
    }
}
