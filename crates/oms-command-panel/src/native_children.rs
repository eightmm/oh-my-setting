use crate::{orchestrator::TASK_PREFIX, text};
use oms_panel_core::{Store, Value, terminals};
use serde_json::json;
use std::path::{Path, PathBuf};

const TRANSCRIPT: &str = ".system_generated/logs/transcript.jsonl";
const MAX_TRANSCRIPT: usize = 4 * 1024 * 1024;

pub fn observe(
    provider: &str,
    pid: u32,
    repo: &Path,
    terminal: &str,
    agy_parent: &mut Option<PathBuf>,
) -> Vec<Value> {
    let Ok(owner) = terminals::process(pid) else {
        return vec![];
    };
    let rows = match provider {
        "claude" => claude(pid, repo),
        "antigravity" => agy(repo, terminal, agy_parent),
        _ => None,
    };
    if terminals::process(pid).is_ok_and(|current| current == owner) {
        rows.unwrap_or_default()
    } else {
        vec![]
    }
}

fn jsonl(store: &Store, path: &Path) -> Option<Vec<Value>> {
    Some(
        store
            .read(path, MAX_TRANSCRIPT, true)
            .ok()?
            .split(|&b| b == b'\n')
            .filter(|line| line.len() <= 256 * 1024)
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect(),
    )
}

fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn safe(s: &str, chars: usize) -> String {
    let bounded = s.chars().take(chars).collect::<String>();
    oms_panel_core::redact(bounded.as_bytes()).unwrap_or_default()
}

fn row(
    provider: &str,
    id: &str,
    title: &str,
    model: &str,
    updated: &str,
    answer: &str,
    done: bool,
) -> Value {
    let hash = oms_panel_core::digest(id.as_bytes());
    json!({
        "participant": format!("native-{provider}-{}", &hash[..16]),
        "alias": "↳",
        "role": "subagent",
        "native_child": true,
        "provider": provider,
        "title": safe(title, 160),
        "model": safe(model, 80),
        "updated_at": updated,
        "created_at": updated,
        "state": if done { "done" } else { "working" },
        "native_answer": safe(answer, 12000),
    })
}

fn claude(pid: u32, repo: &Path) -> Option<Vec<Value>> {
    let home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
    let store = Store::open(&home).ok()?;
    let meta = store
        .json(Path::new(&format!("sessions/{pid}.json")), 16 * 1024, true)
        .ok()??;
    let session = text(&meta, "sessionId");
    if meta["pid"].as_u64() != Some(u64::from(pid))
        || !uuid(session)
        || Path::new(text(&meta, "cwd")).canonicalize().ok().as_deref() != Some(repo)
    {
        return None;
    }
    let project_key: String = repo
        .to_str()?
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let folder = Path::new("projects").join(project_key);
    let rows = jsonl(&store, &folder.join(format!("{session}.jsonl")))?;
    if rows.is_empty() {
        return None;
    }
    let mut tools = std::collections::BTreeSet::new();
    let mut children = Vec::new();
    for entry in &rows {
        if entry["sessionId"] != session {
            continue;
        }
        for block in crate::rows(&entry["message"]["content"]) {
            if block["type"] == "tool_use" && block["name"] == "Agent" {
                tools.insert(text(block, "id"));
            }
            let result = &entry["toolUseResult"];
            let id = text(result, "agentId");
            if block["type"] != "tool_result"
                || !tools.contains(text(block, "tool_use_id"))
                || !(8..=40).contains(&id.len())
                || !id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                continue;
            }
            children.push((
                id,
                result,
                text(entry, "timestamp"),
                text(block, "tool_use_id"),
            ));
        }
    }
    Some(
        children
            .into_iter()
            .rev()
            .take(3)
            .map(|(id, result, started, tool_id)| {
                let path = folder
                    .join(session)
                    .join("subagents")
                    .join(format!("agent-{id}.jsonl"));
                let child = jsonl(&store, &path).unwrap_or_default();
                let blocks = || {
                    child
                        .iter()
                        .rev()
                        .filter(|msg| {
                            msg["type"] == "assistant"
                                && msg["agentId"] == id
                                && msg["sessionId"] == session
                        })
                        .flat_map(|msg| crate::rows(&msg["message"]["content"]).iter().rev())
                };
                let handback = blocks()
                    .find(|block| {
                        block["type"] == "tool_use" && block["name"] == "SubagentHandback"
                    })
                    .and_then(|block| block["input"]["message"].as_str())
                    .filter(|message| !message.is_empty());
                let answer = blocks()
                    .find(|block| block["type"] == "text" && !text(block, "text").is_empty())
                    .map(|block| text(block, "text"));
                let tag = format!("<task-id>{id}</task-id>");
                let tool_tag = format!("<tool-use-id>{tool_id}</tool-use-id>");
                let finished = rows.iter().rev().find(|msg| {
                    msg["sessionId"] == session
                        && msg["type"] == "user"
                        && msg["message"]["content"].as_str().is_some_and(|s| {
                            s.contains("<task-notification>")
                                && s.contains(&tag)
                                && s.contains(&tool_tag)
                                && s.contains("<status>completed</status>")
                        })
                });
                let delivered = finished
                    .and_then(|msg| msg["message"]["content"].as_str())
                    .and_then(|message| message.split_once("<result>"))
                    .and_then(|(_, result)| result.split_once("</result>"))
                    .map(|(result, _)| result.trim())
                    .filter(|result| !result.is_empty());
                row(
                    "claude",
                    id,
                    text(result, "description"),
                    text(result, "resolvedModel"),
                    finished
                        .map(|msg| text(msg, "timestamp"))
                        .unwrap_or(started),
                    handback.or(delivered).or(answer).unwrap_or(""),
                    finished.is_some(),
                )
            })
            .collect(),
    )
}

pub(crate) fn agy_task_anchor(terminal: &str) -> Option<String> {
    let prompt = terminal
        .lines()
        .rev()
        .find_map(|line| {
            line.trim_start()
                .strip_prefix("> ")
                .filter(|s| s.starts_with(TASK_PREFIX))
        })?
        .trim_end();
    let anchor: String = prompt.chars().take(200).collect();
    if anchor.len() < TASK_PREFIX.len() + 45 {
        return None;
    }
    Some(anchor)
}

fn agy(repo: &Path, terminal: &str, parent_file: &mut Option<PathBuf>) -> Option<Vec<Value>> {
    let anchor = agy_task_anchor(terminal)?;
    let home = PathBuf::from(std::env::var_os("HOME")?).join(".gemini/antigravity-cli/brain");
    let store = Store::open(&home).ok()?;
    let matches_prompt = |rows: &[Value]| {
        rows.iter().any(|row| {
            row["type"] == "USER_INPUT"
                && row["source"] == "USER_EXPLICIT"
                && text(row, "content")
                    .lines()
                    .any(|line| line.starts_with(&anchor))
        })
    };
    let rows = if let Some(cached) = parent_file
        .as_ref()
        .and_then(|file| jsonl(&store, file))
        .filter(|rows| matches_prompt(rows))
    {
        cached
    } else {
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(&home).ok()?.take(2048).flatten() {
            let file = entry.file_name();
            if !file.to_str().is_some_and(uuid) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let source = Path::new(&file).join(TRANSCRIPT);
            if let Ok(meta) = std::fs::symlink_metadata(home.join(&source))
                && meta.is_file()
                && meta.len() <= MAX_TRANSCRIPT as u64
                && let Ok(when) = meta.modified()
            {
                candidates.push((when, source));
            }
        }
        candidates.sort_unstable_by(|a, b| b.cmp(a));
        let mut parent = None;
        for (_, path) in candidates.into_iter().take(24) {
            let Some(rows) = jsonl(&store, &path) else {
                continue;
            };
            if matches_prompt(&rows) && parent.replace((rows, path)).is_some() {
                return None;
            }
        }
        let (rows, file) = parent?;
        *parent_file = Some(file);
        rows
    };
    let workspace = format!("file://{}", repo.display());
    let mut pending = None;
    let mut children = Vec::new();
    for entry in &rows {
        if entry["source"] != "MODEL" {
            continue;
        }
        if entry["type"] == "PLANNER_RESPONSE" {
            pending = crate::rows(&entry["tool_calls"])
                .iter()
                .find(|tool| tool["name"] == "invoke_subagent");
        }
        if entry["type"] == "GENERIC"
            && let Some(tool) = pending
            && let Some((_, body)) =
                text(entry, "content").split_once("Created the following subagent:\n")
            && let Some((body, _)) = body.split_once("\n}")
            && let Ok(event) = serde_json::from_str::<Value>(&format!("{body}\n}}"))
        {
            pending = None;
            let id = text(&event, "conversationId");
            if uuid(id)
                && crate::rows(&event["workspaceUris"])
                    .iter()
                    .any(|uri| uri == &workspace)
            {
                children.push((id.to_owned(), agy_title(tool), text(entry, "created_at")));
            }
        }
    }
    Some(
        children
            .into_iter()
            .rev()
            .take(3)
            .map(|(id, title, started)| {
                let child = jsonl(&store, &Path::new(&id).join(TRANSCRIPT)).unwrap_or_default();
                let model = child
                    .iter()
                    .rev()
                    .find_map(|msg| msg["model"].as_str())
                    .unwrap_or("");
                let final_reply = child.iter().rev().find(|msg| {
                    msg["type"] == "PLANNER_RESPONSE"
                        && msg["source"] == "MODEL"
                        && msg["status"] == "DONE"
                        && !text(msg, "content").is_empty()
                });
                let sender = format!(" sender={id} ");
                let finished = final_reply.and_then(|_| {
                    rows.iter()
                        .rev()
                        .filter(|msg| msg["type"] == "SYSTEM_MESSAGE" && msg["source"] == "SYSTEM")
                        .find_map(|msg| {
                            agy_delivery(text(msg, "content"), &sender).map(|body| (msg, body))
                        })
                });
                row(
                    "antigravity",
                    &id,
                    &title,
                    model,
                    finished
                        .map(|(msg, _)| text(msg, "created_at"))
                        .unwrap_or(started),
                    finished
                        .map(|(_, body)| body)
                        .or_else(|| final_reply.map(|msg| text(msg, "content")))
                        .unwrap_or(""),
                    finished.is_some(),
                )
            })
            .collect(),
    )
}

fn agy_delivery<'a>(content: &'a str, sender: &str) -> Option<&'a str> {
    content.rsplit("\n[Message] ").find_map(|part| {
        let (header, body) = part.split_once(" content=")?;
        if !header.starts_with("timestamp=") || !header.contains(sender) {
            return None;
        }
        Some(body.split("</SYSTEM_MESSAGE>").next()?.trim())
    })
}

fn agy_title(tool: &Value) -> String {
    let args = &tool["args"];
    args["Role"]
        .as_str()
        .or_else(|| args["role"].as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Native research")
        .trim_matches('"')
        .to_owned()
}
