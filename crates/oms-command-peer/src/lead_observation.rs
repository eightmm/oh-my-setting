//! Optional provider-reported identity for one successfully published, protected
//! panel Lead answer. None of this data selects an account or resumes a session.
use crate::{Context, V, obj, storage};
use oms_json::Value;
use oms_safe_fs::{ByteReadOptions, Root};
use std::path::Path;

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
    if raw.len() > 2 * 1024 * 1024 {
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
    for (index, line) in std::str::from_utf8(raw)
        .ok()?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        if index >= 2048 {
            return None;
        }
        let row = oms_json::parse_strict(line.as_bytes()).ok()?;
        if crate::providers::native_stopped_early(&row) {
            return None;
        }
        let kind = string(&row, "type")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
