use crate::{
    cli::Options,
    runtime::{self as r, Result},
};
use oms_command_operations::{
    StartRequest, append_lifecycle, create_attempt, load_projection, new_event,
    value::{V, obj},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::{io::Read, path::Path, sync::atomic::AtomicBool};

fn sha256_hex(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(120).collect()
}
fn safe_ref(s: &str) -> Option<String> {
    let s: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    if b.starts_with(b"/")
        || b.starts_with(b"\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'))
        || s.split('/').any(|p| p == "..")
    {
        return None;
    }
    Some(s)
}
fn field<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn transition(repo: &Path, id: &str, state: &str, flag: &AtomicBool) -> bool {
    if let Ok((_, rows)) = load_projection(repo)
        && rows.iter().any(|row| {
            row.get("attempt_id").text() == Some(id) && row.get("state").text() == Some(state)
        })
    {
        return true;
    }
    let fields = obj([
        ("from_state", V::Null),
        ("to_state", state.into()),
        (
            "actor",
            obj([("kind", "owner".into()), ("name", "agy-hook".into())]),
        ),
    ]);
    new_event(id, 0usize.into(), "attempt.state_changed", fields)
        .and_then(|e| append_lifecycle(repo, e, flag))
        .is_ok()
}
fn attempt_state(repo: &Path, id: &str) -> Option<String> {
    let (_, rows) = load_projection(repo).ok()?;
    rows.iter()
        .find(|row| row.get("attempt_id").text() == Some(id))
        .and_then(|row| row.get("state").text())
        .map(str::to_owned)
}
fn ensure_working(repo: &Path, id: &str, flag: &AtomicBool) -> bool {
    match attempt_state(repo, id).as_deref() {
        Some("working") => true,
        Some("starting" | "waiting_input" | "waiting_approval") => {
            transition(repo, id, "working", flag)
        }
        None | Some("queued") => {
            transition(repo, id, "starting", flag) && transition(repo, id, "working", flag)
        }
        Some(_) => false,
    }
}
fn is_terminal(row: &oms_command_operations::value::V) -> bool {
    matches!(
        row.get("state").text(),
        Some("done" | "failed" | "cancelled" | "timed_out" | "abandoned" | "guard_stop")
    )
}
fn is_active_main(row: &oms_command_operations::value::V) -> bool {
    row.get("provider").text() == Some("antigravity")
        && row.get("refs").get("panel_role").text() == Some("main")
        && !is_terminal(row)
}
fn record_child_conversation(repo: &Path, id: &str, conv: &str, flag: &AtomicBool) {
    let Some(safe_conv) = safe_ref(conv) else {
        return;
    };
    let fields = obj([
        (
            "actor",
            obj([("kind", "owner".into()), ("name", "agy-hook".into())]),
        ),
        ("refs", obj([("agy_child_conversation", safe_conv.into())])),
    ]);
    let _ = new_event(id, 0usize.into(), "attempt.heartbeat", fields)
        .and_then(|e| append_lifecycle(repo, e, flag));
}
fn reconcile_from_projection(
    repo: &Path,
    calls: &mut Vec<Value>,
    mains: &mut serde_json::Map<String, Value>,
    flag: &AtomicBool,
) -> Vec<oms_command_operations::value::V> {
    let rows = load_projection(repo)
        .ok()
        .map(|(_, r)| r)
        .unwrap_or_default();
    calls.retain(|e| {
        let aid = field(e, "attempt_id");
        !aid.is_empty()
            && !rows
                .iter()
                .any(|r| r.get("attempt_id").text() == Some(aid) && is_terminal(r))
    });
    for row in &rows {
        if is_terminal(row) || row.get("provider").text() != Some("antigravity") {
            continue;
        }
        let refs = row.get("refs");
        if refs.get("panel_harness").text() != Some("agy") {
            continue;
        }
        let Some(call_id) = refs
            .get("agy_tool_call_id")
            .text()
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(attempt_id) = row.get("attempt_id").text().filter(|s| !s.is_empty()) else {
            continue;
        };
        let _ = ensure_working(repo, attempt_id, flag);
        let parent_attempt = row.get("parent_attempt_id").text().unwrap_or("");
        let parent_conv = refs.get("agy_parent_conversation").text().unwrap_or("");
        let child_conv = refs
            .get("agy_child_conversation")
            .text()
            .filter(|s| !s.is_empty());
        let room = row_room(row);
        let kind = refs
            .get("agy_kind")
            .text()
            .unwrap_or_else(|| match row.get("tool").text() {
                Some("agy-swarm") => "swarm",
                Some("agy-workflow") => "workflow",
                _ => "subagent",
            });
        let swarm_name = refs.get("agy_swarm_name").text().unwrap_or("");
        let prompt_sha256 = refs.get("agy_prompt_sha256").text().unwrap_or("");
        if !parent_conv.is_empty()
            && !parent_attempt.is_empty()
            && rows
                .iter()
                .any(|m| m.get("attempt_id").text() == Some(parent_attempt) && is_active_main(m))
        {
            mains
                .entry(parent_conv.to_owned())
                .or_insert_with(|| json!(parent_attempt));
        }
        if let Some(existing) = calls.iter_mut().find(|e| {
            field(e, "attempt_id") == attempt_id
                || (field(e, "call_id") == call_id
                    && field(e, "parent_conversation") == parent_conv)
        }) {
            if field(existing, "parent_attempt_id").is_empty() && !parent_attempt.is_empty() {
                existing["parent_attempt_id"] = json!(parent_attempt);
            }
            if field(existing, "room").is_empty() && !room.is_empty() {
                existing["room"] = json!(room);
            }
            if existing["child_conversation"].is_null()
                && let Some(c) = child_conv
            {
                existing["child_conversation"] = json!(c);
            }
        } else {
            calls.push(json!({
                "call_id": call_id,
                "attempt_id": attempt_id,
                "parent_attempt_id": parent_attempt,
                "room": room,
                "parent_conversation": parent_conv,
                "child_conversation": child_conv.map_or(Value::Null, |c| json!(c)),
                "kind": kind,
                "swarm_name": swarm_name,
                "prompt_sha256": prompt_sha256,
            }));
        }
    }
    rows
}
fn row_room(row: &oms_command_operations::value::V) -> String {
    row.get("refs")
        .get("panel_room_id")
        .text()
        .or_else(|| row.get("refs").get("panel_room").text())
        .unwrap_or("")
        .into()
}
fn row_claims_session(row: &oms_command_operations::value::V) -> bool {
    let refs = row.get("refs");
    [
        "native_session",
        "panel_native_session",
        "agy_conversation",
        "consumer",
        "panel_session_digest",
    ]
    .iter()
    .any(|k| refs.get(k).text().is_some_and(|v| !v.is_empty()))
}
fn row_owns_conversation(
    row: &oms_command_operations::value::V,
    conv: &str,
    conv_digest: &str,
) -> bool {
    if conv.is_empty() {
        return false;
    }
    let refs = row.get("refs");
    refs.get("native_session").text() == Some(conv)
        || refs.get("panel_native_session").text() == Some(conv)
        || refs.get("agy_conversation").text() == Some(conv)
        || (!conv_digest.is_empty()
            && (refs.get("consumer").text() == Some(conv_digest)
                || refs.get("panel_session_digest").text() == Some(conv_digest)))
}
fn resolve_context(
    repo: &Path,
    conv: &str,
    mains: &mut serde_json::Map<String, Value>,
) -> Option<(String, String)> {
    let env_parent = std::env::var("OMS_PANEL_MAIN_ATTEMPT").unwrap_or_default();
    let env_room = std::env::var("OMS_ROOM_ID").unwrap_or_default();
    let conv_digest = if conv.is_empty() {
        String::new()
    } else {
        sha256_hex(conv).get(..32).unwrap_or("").to_owned()
    };
    let rows = load_projection(repo)
        .ok()
        .map(|(_, r)| r)
        .unwrap_or_default();
    if !env_parent.is_empty() {
        let row = rows
            .iter()
            .find(|v| v.get("attempt_id").text() == Some(env_parent.as_str()))?;
        if !is_active_main(row)
            || (!conv.is_empty()
                && row_claims_session(row)
                && !row_owns_conversation(row, conv, &conv_digest))
        {
            return None;
        }
        let room = if !env_room.is_empty() {
            env_room
        } else {
            row_room(row)
        };
        if !conv.is_empty() {
            mains.insert(conv.into(), json!(env_parent));
        }
        return Some((env_parent, room));
    }
    if conv.is_empty() {
        return None;
    }
    let row = rows.iter().rev().find(|v| {
        if !is_active_main(v) {
            return false;
        }
        let room = row_room(v);
        if !env_room.is_empty() && !room.is_empty() && room != env_room {
            return false;
        }
        row_owns_conversation(v, conv, &conv_digest)
            || (!row_claims_session(v)
                && mains.get(conv).and_then(Value::as_str) == v.get("attempt_id").text())
    })?;
    let parent: String = row.get("attempt_id").text().unwrap_or("").into();
    if parent.is_empty() {
        return None;
    }
    let room = if !env_room.is_empty() {
        env_room
    } else {
        row_room(row)
    };
    mains.insert(conv.into(), json!(parent));
    Some((parent, room))
}
fn track(repo: &Path, action: &str, payload: &Value, flag: &AtomicBool) -> Result<bool> {
    let dir = repo.join(".oms/panel-state");
    fs::create_dir_all(&dir)?;
    let path = dir.join("agy-native-calls.json");
    let _lock = r::lock(&path, repo)?;
    let mut state = r::value(&path)
        .ok()
        .filter(|v| v["calls"].is_array())
        .unwrap_or_else(|| json!({"schema":1,"calls":[]}));
    let mut mains = state["mains"].as_object().cloned().unwrap_or_default();
    let Some(mut calls) = state["calls"].as_array().cloned() else {
        return Ok(false);
    };
    let rows = reconcile_from_projection(repo, &mut calls, &mut mains, flag);
    let conv = field(payload, "conversationId");
    let mut is_child = !conv.is_empty()
        && (calls
            .iter()
            .any(|e| e["child_conversation"].as_str() == Some(conv))
            || rows
                .iter()
                .any(|r| r.get("refs").get("agy_child_conversation").text() == Some(conv)));
    let call = &payload["toolCall"];
    let id = field(call, "id");
    let args = &call["args"];
    if action == "preinvocation" && !conv.is_empty() {
        let conv_digest = sha256_hex(conv).get(..32).unwrap_or("").to_owned();
        let env_parent = std::env::var("OMS_PANEL_MAIN_ATTEMPT").unwrap_or_default();
        let env_room = std::env::var("OMS_ROOM_ID").unwrap_or_default();
        let claimed_by_main = mains.contains_key(conv)
            || rows
                .iter()
                .any(|m| is_active_main(m) && row_owns_conversation(m, conv, &conv_digest));
        if !claimed_by_main
            && payload["invocationNum"].as_i64().unwrap_or(1) <= 1
            && !field(payload, "lastUserInput").is_empty()
            && !calls.iter().any(|e| {
                field(e, "child_conversation") == conv || field(e, "parent_conversation") == conv
            })
        {
            let input_hash = sha256_hex(field(payload, "lastUserInput"));
            let eligible_pending = |e: &Value| -> bool {
                if !e["child_conversation"].is_null()
                    || field(e, "kind") != "subagent"
                    || field(e, "prompt_sha256") != input_hash
                {
                    return false;
                }
                let parent_id = field(e, "parent_attempt_id");
                let Some(parent_row) = rows
                    .iter()
                    .find(|m| m.get("attempt_id").text() == Some(parent_id) && is_active_main(m))
                else {
                    return false;
                };
                if !env_parent.is_empty() && parent_id != env_parent {
                    return false;
                }
                let entry_room = field(e, "room");
                if !env_room.is_empty() && !entry_room.is_empty() && entry_room != env_room {
                    return false;
                }
                let parent_room = if !entry_room.is_empty() {
                    entry_room.to_owned()
                } else {
                    row_room(parent_row)
                };
                if !env_room.is_empty() && !parent_room.is_empty() && parent_room != env_room {
                    return false;
                }
                if env_parent.is_empty() {
                    let other_unbound_main = rows.iter().any(|m| {
                        let aid = m.get("attempt_id").text().unwrap_or("");
                        if !is_active_main(m) || aid == parent_id {
                            return false;
                        }
                        let mr = row_room(m);
                        if !env_room.is_empty() && !mr.is_empty() && mr != env_room {
                            return false;
                        }
                        !row_claims_session(m) && !mains.values().any(|v| v.as_str() == Some(aid))
                    });
                    if other_unbound_main {
                        return false;
                    }
                }
                true
            };
            if calls.iter().filter(|e| eligible_pending(e)).count() == 1
                && let Some(entry) = calls.iter_mut().find(|e| eligible_pending(e))
            {
                entry["child_conversation"] = json!(conv);
                record_child_conversation(repo, field(entry, "attempt_id"), conv, flag);
                is_child = true;
            }
        }
        if !is_child {
            let _ = resolve_context(repo, conv, &mut mains);
        }
    } else if action == "pretooluse" {
        let name = field(call, "name");
        let spec = match name {
            "invoke_subagent" => {
                let typ = clean(field(args, "TypeName"));
                let typ = if typ.is_empty() {
                    "subagent".to_string()
                } else {
                    typ
                };
                let role = clean(field(args, "Role"));
                let label = if role.is_empty() { &typ } else { &role };
                let model = field(args, "Model");
                let model = if model.is_empty() || model == "inherit" {
                    let inherited = field(payload, "modelName");
                    if inherited == "auto" { "" } else { inherited }
                } else {
                    model
                };
                let prompt = field(args, "Prompt");
                let prompt_hash = if prompt.is_empty() {
                    String::new()
                } else {
                    sha256_hex(prompt)
                };
                Some((
                    "subagent",
                    "agy-subagent",
                    if typ == "research" {
                        "researcher"
                    } else {
                        "worker"
                    },
                    typ.clone(),
                    format!("[agy:{typ}] {label}"),
                    "".to_string(),
                    clean(model),
                    prompt_hash,
                ))
            }
            "swarm" if field(args, "action") == "add" => {
                let n = clean(field(args, "name"));
                let n = if n.is_empty() { "swarm".to_string() } else { n };
                let count = args["tasks"]
                    .as_array()
                    .map(|a| a.len() as u64)
                    .or_else(|| args["n"].as_u64())
                    .unwrap_or(1);
                let label = if count > 1 {
                    format!("[agy:swarm] {n} (x{count})")
                } else {
                    format!("[agy:swarm] {n}")
                };
                Some((
                    "swarm",
                    "agy-swarm",
                    "worker",
                    "swarm".into(),
                    label,
                    n,
                    String::new(),
                    String::new(),
                ))
            }
            "run_workflow" => {
                let desc = clean(field(args, "Description"));
                let desc = if desc.is_empty() { "workflow" } else { &desc };
                Some((
                    "workflow",
                    "agy-workflow",
                    "worker",
                    "workflow".into(),
                    format!("[agy:workflow] {desc}"),
                    String::new(),
                    String::new(),
                    String::new(),
                ))
            }
            _ => None,
        };
        if let Some((kind, tool, role, strategy, label, swarm, model, prompt_hash)) = spec
            && !id.is_empty()
            && !calls
                .iter()
                .any(|e| field(e, "call_id") == id && field(e, "parent_conversation") == conv)
            && let Some((parent, room)) = resolve_context(repo, conv, &mut mains)
        {
            let mut refs = obj([
                ("panel_role", role.into()),
                ("panel_provider", "antigravity".into()),
                ("panel_harness", "agy".into()),
                ("panel_strategy", strategy.into()),
                ("panel_label", clean(&label).into()),
                ("agy_kind", kind.into()),
            ]);
            for (key, value) in [
                ("panel_model", safe_ref(&model)),
                ("panel_room_id", safe_ref(&room)),
                ("agy_parent_conversation", safe_ref(conv)),
                ("agy_tool_call_id", safe_ref(id)),
                ("agy_swarm_name", safe_ref(&swarm)),
                ("agy_prompt_sha256", safe_ref(&prompt_hash)),
            ] {
                if let Some(value) = value {
                    refs.set(key, value);
                }
            }
            let request = StartRequest {
                provider: "antigravity".into(),
                tool: tool.into(),
                parent_attempt_id: parent.clone(),
                refs,
                idempotency_key: format!("agy-pretool:{conv}:{id}"),
                ..Default::default()
            };
            if let Ok(attempt) = create_attempt(repo, &request, flag)
                && ensure_working(repo, &attempt, flag)
            {
                calls.push(json!({"call_id":id,"attempt_id":attempt,"parent_attempt_id":parent,"room":room,"parent_conversation":conv,"child_conversation":null,"kind":kind,"swarm_name":swarm,"prompt_sha256":prompt_hash}));
            }
        }
    } else if action == "posttooluse" {
        if field(payload, "error").is_empty() {
            let name = field(call, "name");
            let cancel = |e: &Value| -> bool {
                if field(e, "parent_conversation") != conv {
                    return false;
                }
                match name {
                    "swarm" if field(args, "action") == "kill" => {
                        field(e, "kind") == "swarm"
                            && (field(args, "name").is_empty()
                                || field(e, "swarm_name") == clean(field(args, "name")))
                    }
                    "manage_subagents" if field(e, "kind") == "subagent" => {
                        match field(args, "Action") {
                            "kill_all" => true,
                            "kill" => args["ConversationIds"].as_array().is_some_and(|ids| {
                                ids.iter().any(|cid| {
                                    cid.as_str().is_some_and(|s| {
                                        !s.is_empty() && Some(s) == e["child_conversation"].as_str()
                                    })
                                })
                            }),
                            _ => false,
                        }
                    }
                    _ => false,
                }
            };
            calls.retain(|e| {
                !cancel(e) || !transition(repo, field(e, "attempt_id"), "cancelled", flag)
            });
        }
        let unique_without_conv = conv.is_empty()
            && !id.is_empty()
            && calls.iter().filter(|e| field(e, "call_id") == id).count() == 1;
        calls.retain(|e| {
            let same_conv = if conv.is_empty() {
                unique_without_conv
            } else {
                field(e, "parent_conversation") == conv
            };
            if field(e, "call_id") != id || id.is_empty() || !same_conv {
                return true;
            }
            let attempt = field(e, "attempt_id");
            if !field(payload, "error").is_empty() {
                !transition(repo, attempt, "failed", flag)
            } else {
                true
            }
        });
    } else if action == "stop" && !conv.is_empty() {
        let idle = payload["fullyIdle"].as_bool().unwrap_or(false);
        let term = field(payload, "terminationReason");
        calls.retain(|e| {
            if !((idle || !term.is_empty()) && e["child_conversation"].as_str() == Some(conv)
                || idle && field(e, "parent_conversation") == conv)
            {
                return true;
            }
            let attempt = field(e, "attempt_id");
            match term {
                "ERROR" | "MAX_INVOCATIONS" => !transition(repo, attempt, "failed", flag),
                "USER_CANCELED" => !transition(repo, attempt, "cancelled", flag),
                _ => {
                    !(transition(repo, attempt, "review", flag)
                        && transition(repo, attempt, "done", flag))
                }
            }
        });
        if idle && !is_child {
            mains.remove(conv);
        }
    }
    state["calls"] = Value::Array(calls);
    if mains.is_empty() {
        if let Some(map) = state.as_object_mut() {
            map.remove("mains");
        }
    } else {
        state["mains"] = Value::Object(mains);
    }
    r::atomic(&path, &r::bytes(&state)?)?;
    Ok(is_child)
}

pub(crate) fn run(
    cwd: &Path,
    source: &Path,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    if !matches!(
        options.action.as_str(),
        "preinvocation" | "pretooluse" | "posttooluse" | "stop"
    ) {
        return r::fail("agy-hook requires preinvocation, pretooluse, posttooluse, or stop");
    }
    let empty = || {
        r::json(&if options.action == "pretooluse" {
            json!({"decision":"allow"})
        } else {
            json!({})
        })
    };
    if std::env::var_os("OH_MY_SETTING_AGY_HOOKS").is_some_and(|s| s == "0")
        || std::env::var_os("OMS_HARNESS_CHILD").is_some_and(|s| s == "1")
    {
        return empty();
    }
    let mut raw = Vec::new();
    std::io::stdin()
        .lock()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 1024 * 1024 {
        return empty();
    }
    let Ok(payload) = r::parse(&raw) else {
        return empty();
    };
    let Some(repo) = payload["workspacePaths"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|p| p.as_str())
        .map(Path::new)
        .filter(|p| p.is_absolute() && p.is_dir())
    else {
        return empty();
    };
    let is_child = if repo.join(".oms").is_dir() {
        track(repo, &options.action, &payload, flag).unwrap_or(false)
    } else {
        false
    };
    if matches!(options.action.as_str(), "pretooluse" | "posttooluse") || is_child {
        return empty();
    }
    let first = options.action == "preinvocation";
    let count = payload[if first {
        "invocationNum"
    } else {
        "executionNum"
    }]
    .as_i64()
    .unwrap_or(0);
    if first && count <= 1 && repo.join(".oms").is_dir() {
        let session = payload["conversationId"].as_str().unwrap_or("");
        let adapted = json!({"cwd":repo,"session_id":session,"turn_id":format!("{session}:{count}"),"hook_event_name":"UserPromptSubmit","model":payload["modelName"],"prompt":""});
        if let Ok(input) = r::bytes(&adapted) {
            let _ = services.provider_hook("telemetry-hook", &input, repo, source, true, flag);
        }
    }
    if first && (count > 1 || std::env::var_os("OMS_SKILL_ROUTER_OFF").is_some_and(|s| s == "1")) {
        return empty();
    }
    let session = payload["conversationId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("nosession");
    let adapted = json!({"cwd":repo,"session_id":session,"turn_id":format!("{session}:{count}"),"hook_event_name":if first{"PreInvocation"}else{"Stop"},"model":payload["modelName"].as_str().unwrap_or(""),"prompt":""});
    let input = r::bytes(&adapted)?;
    let mut diagnostics = Vec::new();
    if first {
        let out = services.provider_hook("skill-router", &input, repo, source, true, flag);
        if out.0 != 0 {
            diagnostics.extend_from_slice(b"native Antigravity router unavailable\n");
        }
        let hint = String::from_utf8(out.1).unwrap_or_default();
        let mut result = if out.0 == 0 && !hint.trim().is_empty() {
            r::json(&json!({"injectSteps":[{"systemMessage":{"systemMessage":hint.trim()}}]}))?
        } else {
            empty()?
        };
        result.2 = diagnostics;
        Ok(result)
    } else {
        for name in ["turn-guard", "telemetry-hook"] {
            let out = services.provider_hook(name, &input, repo, source, true, flag);
            if out.0 != 0 {
                diagnostics.extend_from_slice(b"native Antigravity observer unavailable\n");
            }
        }
        let mut result = empty()?;
        result.2 = diagnostics;
        let _ = cwd;
        Ok(result)
    }
}
