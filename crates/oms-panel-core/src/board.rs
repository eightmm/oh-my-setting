//! Read-only Board projection for active/external Main identity and peer Activity rows.
//!
//! Combines:
//! - `.oms/artifacts/mcp/<op>/` peer operation records (via `oms_command_operations::peer_operations`),
//! - unroomed canonical `peer-delegate` / `peer-review` / peer consultation lifecycle attempts,
//! - verified managed scratch worktree peer runs,
//! - Room `panel-main` + `.oms/hooks/panel-activity/<member>.json` idle/busy observations.
use crate::scratch_activity::ScratchPeerRun;
use crate::state::{LIVE_STATES, main_participant};
use crate::value::{array, text};
use crate::{Store, Value};
use oms_command_operations::peer_operations::{
    PeerOperationRecord, is_mcp_operation_id, list_peer_operations,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

pub const UNRECORDED_NATIVE_TASK: &str = "Native task not recorded";
pub const MAX_ACTIVITY_ROWS: usize = 20;
const HOOK_ACTIVITY_LIMIT: usize = 4096;
const HOOK_EVENTS_TAIL_LIMIT: u64 = 16 * 1024;
const HOOK_CALLER_WINDOW_SECS: i64 = 1800;

const PEER_TOOLS: &[&str] = &[
    "peer-delegate",
    "peer-review",
    "peer-ask",
    "consult",
    "advise",
    "ask",
];

fn normalize_provider(raw: &str) -> &str {
    match raw.trim() {
        "agy" => "antigravity",
        "cursor-agent" => "cursor",
        "grok-build" => "grok",
        "gemini-cli" => "gemini",
        "qwen-code" => "qwen",
        other => other,
    }
}

fn provider_display_name(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude",
        "antigravity" | "agy" => "Antigravity",
        "pi" => "Pi",
        other => other,
    }
}

fn parse_target_spec(target: &str) -> (String, String) {
    let (raw_provider, spec) = target
        .split_once(':')
        .map_or((target, None), |(p, rest)| (p, Some(rest)));
    let provider = normalize_provider(raw_provider).to_owned();
    let model = spec
        .and_then(|s| s.strip_prefix("model="))
        .map(str::trim)
        .filter(|m| !m.is_empty() && !m.chars().any(char::is_control))
        .unwrap_or("")
        .to_owned();
    (provider, model)
}

fn canonical_tool_and_role(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "peer-delegate" | "delegate" => ("peer-delegate", "delegate"),
        "peer-review" | "review" => ("peer-review", "reviewer"),
        "peer-ask" | "ask" => ("peer-ask", "advisor"),
        "consult" => ("consult", "advisor"),
        "advise" => ("advise", "advisor"),
        _ => ("peer-delegate", "worker"),
    }
}

fn safe_worktree_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()
        && trimmed.len() <= 160
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains(['/', '\\'])
        && !trimmed.chars().any(char::is_control))
    .then(|| trimmed.to_owned())
}

pub fn repo_worktree_label(repo: &Path) -> String {
    repo.file_name()
        .and_then(|n| n.to_str())
        .and_then(safe_worktree_name)
        .unwrap_or_else(|| "workspace".to_owned())
}

pub fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        let mins = seconds / 60;
        let secs = seconds % 60;
        if secs == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m {secs}s")
        }
    } else {
        let hours = seconds / 3600;
        let mins = (seconds % 3600) / 60;
        if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {mins}m")
        }
    }
}

fn parse_epoch(stamp: &str) -> Option<i64> {
    oms_command_operations::parse_ts(stamp.trim())
        .ok()
        .map(|dt| dt.timestamp())
}

fn is_active_peer_state(state: &str) -> bool {
    matches!(
        state,
        "running" | "working" | "starting" | "queued" | "verifying"
    )
}

pub(crate) fn is_live_or_stalled(state: &str) -> bool {
    is_active_peer_state(state) || state == "stalled"
}

fn peer_operation_row(
    rec: &PeerOperationRecord,
    default_worktree: &str,
    panel_main_attempt: &str,
    now_epoch: i64,
) -> Value {
    let (tool, role) = canonical_tool_and_role(&rec.kind);
    let (provider, model) = rec
        .targets
        .first()
        .map(|t| parse_target_spec(t))
        .unwrap_or_else(|| ("unknown".to_owned(), String::new()));
    let worktree = rec
        .worktree
        .as_deref()
        .and_then(safe_worktree_name)
        .unwrap_or_else(|| default_worktree.to_owned());
    let started_epoch = parse_epoch(&rec.started_at);
    let elapsed_secs = if rec.status == "running" {
        match (started_epoch, rec.age_seconds) {
            (Some(ts), Some(0)) if ts > 0 && ts <= now_epoch && now_epoch > ts => {
                Some((now_epoch - ts) as u64)
            }
            (_, Some(age)) if age >= 0 => Some(age as u64),
            (Some(ts), _) if ts > 0 && ts <= now_epoch => Some((now_epoch - ts) as u64),
            _ => None,
        }
    } else {
        let end_epoch = rec.finished_at.as_deref().and_then(parse_epoch);
        match (started_epoch, end_epoch) {
            (Some(s), Some(e)) if s > 0 && e >= s => Some((e - s) as u64),
            _ => None,
        }
    };
    let elapsed = elapsed_secs.map(format_elapsed).unwrap_or_default();
    let title = rec.title.clone().unwrap_or_default();
    let state = if rec.status == "done" {
        if rec.cancelled {
            "cancelled"
        } else if rec.exit.is_some_and(|code| code != 0) {
            "failed"
        } else {
            "done"
        }
    } else {
        rec.status
    };
    let origin =
        if !panel_main_attempt.is_empty() && rec.caller.as_deref() == Some(panel_main_attempt) {
            "panel"
        } else {
            "external"
        };
    json!({
        "id": format!("mcp:{}", rec.operation),
        "source": "peer_operation",
        "origin": origin,
        "operation": rec.operation,
        "operation_status": rec.status,
        "attempt_id": rec.attempt_id.clone().unwrap_or_default(),
        "tool": tool,
        "role": role,
        "provider": provider,
        "model": model,
        "targets": rec.targets,
        "worktree": worktree,
        "state": state,
        "exit": rec.exit,
        "started_at": rec.started_at,
        "updated_at": rec.updated_at,
        "elapsed_seconds": elapsed_secs,
        "elapsed": elapsed,
        "title": title,
        "caller": rec.caller,
    })
}

fn lifecycle_attempt_row(
    attempt: &Value,
    room_id: &str,
    panel_main_attempt: &str,
    default_worktree: &str,
    now_epoch: i64,
) -> Option<Value> {
    let tool = text(attempt, "tool");
    if !PEER_TOOLS.contains(&tool.as_str()) {
        return None;
    }
    let refs = &attempt["refs"];
    let rooms = [text(refs, "panel_room_id"), text(refs, "panel_room")];
    if !room_id.is_empty() && rooms.iter().any(|r| !r.is_empty() && r != room_id) {
        return None;
    }
    let attempt_id = text(attempt, "attempt_id");
    if attempt_id.is_empty() {
        return None;
    }
    let parent_attempt = text(attempt, "parent_attempt_id");
    let bound_to_panel = (!panel_main_attempt.is_empty() && parent_attempt == panel_main_attempt)
        || (!room_id.is_empty() && rooms.iter().any(|r| r == room_id))
        || (room_id.is_empty() && rooms.iter().any(|r| !r.is_empty()));
    let origin = if bound_to_panel { "panel" } else { "external" };
    let (canonical_tool, default_role) = canonical_tool_and_role(&tool);
    let ref_role = text(refs, "panel_role");
    let role = if ref_role.is_empty() {
        default_role.to_owned()
    } else {
        ref_role
    };
    let raw_provider = text(attempt, "provider");
    let provider = if raw_provider.is_empty() {
        "unknown".to_owned()
    } else {
        normalize_provider(&raw_provider).to_owned()
    };
    let model = text(refs, "panel_model");
    let ref_worktree = text(refs, "worktree");
    let worktree = if ref_worktree != "tree" {
        safe_worktree_name(&ref_worktree).unwrap_or_else(|| default_worktree.to_owned())
    } else {
        default_worktree.to_owned()
    };
    let state = text(attempt, "state");
    let created_at = text(attempt, "created_at");
    let updated_at = text(attempt, "updated_at");
    let ended_at = text(attempt, "ended_at");
    let elapsed_secs = if LIVE_STATES.contains(&state.as_str()) {
        parse_epoch(&created_at)
            .filter(|&ts| ts > 0 && ts <= now_epoch)
            .map(|ts| (now_epoch - ts) as u64)
    } else if let Some(ms) = attempt["usage_reports"]["duration_ms"]
        .as_u64()
        .filter(|&c| c > 0)
        .and_then(|_| attempt["usage"]["duration_ms"].as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
    {
        Some((ms / 1000.0) as u64)
    } else {
        let start = parse_epoch(&created_at);
        let end = parse_epoch(if ended_at.is_empty() {
            &updated_at
        } else {
            &ended_at
        });
        match (start, end) {
            (Some(s), Some(e)) if s > 0 && e >= s => Some((e - s) as u64),
            _ => None,
        }
    };
    let elapsed = elapsed_secs.map(format_elapsed).unwrap_or_default();
    let title = [text(attempt, "title"), text(refs, "panel_label")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let caller = [
        text(refs, "caller_provider"),
        text(refs, "main_provider"),
        text(refs, "panel_owner"),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .and_then(|s| safe_worktree_name(&s));
    Some(json!({
        "id": format!("attempt:{attempt_id}"),
        "source": "lifecycle",
        "origin": origin,
        "operation": "",
        "attempt_id": attempt_id,
        "tool": canonical_tool,
        "role": role,
        "provider": provider,
        "model": model,
        "targets": [provider],
        "worktree": worktree,
        "state": state,
        "exit": Value::Null,
        "started_at": created_at,
        "updated_at": if updated_at.is_empty() { created_at } else { updated_at },
        "elapsed_seconds": elapsed_secs,
        "elapsed": elapsed,
        "title": title,
        "caller": caller,
    }))
}

fn scratch_peer_row(run: &ScratchPeerRun, now_epoch: i64) -> Value {
    let (tool, role) = canonical_tool_and_role(run.tool);
    let provider = run
        .provider
        .map(normalize_provider)
        .unwrap_or("unknown")
        .to_owned();
    let elapsed_secs = if LIVE_STATES.contains(&run.state.as_str()) {
        parse_epoch(&run.created_at)
            .filter(|&ts| ts > 0 && ts <= now_epoch)
            .map(|ts| (now_epoch - ts) as u64)
    } else {
        match (parse_epoch(&run.created_at), parse_epoch(&run.updated_at)) {
            (Some(s), Some(e)) if s > 0 && e >= s => Some((e - s) as u64),
            _ => None,
        }
    };
    let elapsed = elapsed_secs.map(format_elapsed).unwrap_or_default();
    json!({
        "id": format!("scratch:{}", &run.id[..16.min(run.id.len())]),
        "source": "scratch_lifecycle",
        "origin": "external",
        "operation": "",
        "attempt_id": "",
        "tool": tool,
        "role": role,
        "provider": provider,
        "model": "",
        "targets": [provider],
        "worktree": run.worktree,
        "state": run.state,
        "exit": Value::Null,
        "started_at": run.created_at,
        "updated_at": run.updated_at,
        "elapsed_seconds": elapsed_secs,
        "elapsed": elapsed,
        "title": "",
        "caller": Value::Null,
    })
}

fn read_hook_activity_state(
    store: &Store,
    room_id: &str,
    participant: &str,
    attempt_id: &str,
) -> (Option<String>, Option<f64>) {
    if participant.is_empty() || attempt_id.is_empty() || crate::identifier(participant).is_err() {
        return (None, None);
    }
    let Some(row) = store
        .json_owned(
            Path::new(&format!(".oms/hooks/panel-activity/{participant}.json")),
            HOOK_ACTIVITY_LIMIT,
        )
        .ok()
        .flatten()
    else {
        return (None, None);
    };
    if row["attempt"].as_str() != Some(attempt_id) {
        return (None, None);
    }
    let row_room = text(&row, "room");
    if !room_id.is_empty() && !row_room.is_empty() && row_room != room_id {
        return (None, None);
    }
    let state = row["state"]
        .as_str()
        .filter(|s| matches!(*s, "idle" | "busy" | "waiting"))
        .map(str::to_owned);
    let updated = state
        .is_some()
        .then(|| {
            crate::metrics::stamp(&row["updated_at"])
                .or_else(|| crate::metrics::stamp(&row["since"]))
        })
        .flatten();
    (state, updated)
}

fn attempt_mcp_operation(attempt: &Value) -> Option<String> {
    let refs = &attempt["refs"];
    let explicit = text(refs, "mcp_operation");
    if is_mcp_operation_id(&explicit) {
        return Some(explicit);
    }
    let artifact = text(refs, "artifact");
    let rest = artifact.strip_prefix(".oms/artifacts/mcp/")?;
    let op = rest.split('/').next()?;
    is_mcp_operation_id(op).then(|| op.to_owned())
}

fn detect_external_hook_caller(
    repo: &Path,
    exclude_provider: Option<&str>,
    now_epoch: i64,
) -> Option<&'static str> {
    use rustix::fs::{self as rfs, FileType, Mode, OFlags};
    use std::collections::BTreeMap;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;

    let dir_flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let uid = rustix::process::getuid().as_raw();
    let mut dir: std::fs::File = rfs::open(repo, dir_flags, Mode::empty()).ok()?.into();
    let root_meta = dir.metadata().ok()?;
    if root_meta.uid() != uid || root_meta.mode() & 0o002 != 0 {
        return None;
    }
    for seg in [".oms", "hooks"] {
        dir = rfs::openat(&dir, seg, dir_flags, Mode::empty())
            .ok()?
            .into();
        let meta = dir.metadata().ok()?;
        if meta.uid() != uid || meta.mode() & 0o002 != 0 {
            return None;
        }
    }
    let fd = rfs::openat(
        &dir,
        "events.jsonl",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let stat = rfs::fstat(&fd).ok()?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != uid
        || stat.st_mode & 0o002 != 0
        || stat.st_size <= 0
    {
        return None;
    }
    let size = stat.st_size as u64;
    let start = size.saturating_sub(HOOK_EVENTS_TAIL_LIMIT);
    let mut file = std::fs::File::from(fd);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(HOOK_EVENTS_TAIL_LIMIT)
        .read_to_end(&mut buf)
        .ok()?;
    let slice = if start > 0 {
        let pos = buf.iter().position(|&b| b == b'\n')?;
        &buf[pos + 1..]
    } else {
        buf.as_slice()
    };
    let mut sessions: BTreeMap<String, (i64, &'static str, bool)> = BTreeMap::new();
    for line in slice.split(|&b| b == b'\n') {
        if line.is_empty() || line.len() > 8192 {
            continue;
        }
        let Ok(val) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(ts) = parse_epoch(&text(&val, "ts")) else {
            continue;
        };
        if ts <= 0 || ts > now_epoch || now_epoch - ts > HOOK_CALLER_WINDOW_SECS {
            continue;
        }
        let provider = match normalize_provider(&text(&val, "agent")) {
            "claude" => "claude",
            "codex" => "codex",
            "antigravity" => "antigravity",
            "pi" => "pi",
            _ => continue,
        };
        if exclude_provider == Some(provider) {
            continue;
        }
        let session = text(&val, "session");
        let key = if session.is_empty() {
            provider.to_owned()
        } else {
            session
        };
        let stopped = matches!(text(&val, "hook").as_str(), "Stop" | "SessionEnd");
        sessions.insert(key, (ts, provider, stopped));
    }
    sessions
        .into_values()
        .filter(|(_, _, stopped)| !*stopped)
        .max_by_key(|(ts, _, _)| *ts)
        .map(|(_, provider, _)| provider)
}

fn select_live_panel_main<'a>(
    room: &Value,
    attempts: &'a Value,
    room_attempts: &'a serde_json::Map<String, Value>,
) -> Option<&'a Value> {
    let room_id = text(room, "id");
    if !room_id.is_empty() {
        if room["closed"] == true || !room["successor"].is_null() {
            return None;
        }
        let owner = room["owner"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| text(room, "main"));
        if !owner.is_empty() {
            if let Some(row) = room_attempts.get(&owner).filter(|row| {
                row["tool"] == "panel-main"
                    && row["terminal"] == false
                    && LIVE_STATES.contains(&text(row, "state").as_str())
            }) {
                return Some(row);
            }
            for member in array(&room["participants"]) {
                if member["role"] == "main"
                    && member["joined"] == true
                    && text(member, "participant") == owner
                {
                    let aid = text(member, "attempt");
                    if !aid.is_empty()
                        && let Some(row) = room_attempts.get(&aid).filter(|row| {
                            row["tool"] == "panel-main"
                                && row["terminal"] == false
                                && LIVE_STATES.contains(&text(row, "state").as_str())
                        })
                    {
                        return Some(row);
                    }
                }
            }
        }
        return room_attempts.values().find(|row| {
            row["tool"] == "panel-main"
                && row["terminal"] == false
                && LIVE_STATES.contains(&text(row, "state").as_str())
        });
    }
    let mut mains = array(&attempts["active"]).iter().filter(|row| {
        row["tool"] == "panel-main"
            && row["terminal"] == false
            && LIVE_STATES.contains(&text(row, "state").as_str())
    });
    let first = mains.next()?;
    mains.next().is_none().then_some(first)
}

fn has_live_bound_room_children(
    room: &Value,
    attempts: &Value,
    room_attempts: &serde_json::Map<String, Value>,
    main_attempt: &str,
    main_participant_id: &str,
) -> bool {
    let room_id = text(room, "id");
    let check = |row: &Value| {
        let aid = text(row, "attempt_id");
        if aid.is_empty()
            || aid == main_attempt
            || row["tool"] == "panel-main"
            || row["terminal"] == true
            || !LIVE_STATES.contains(&text(row, "state").as_str())
        {
            return false;
        }
        if text(row, "parent_attempt_id") == main_attempt {
            return true;
        }
        let refs = &row["refs"];
        !room_id.is_empty()
            && (text(refs, "panel_room_id") == room_id || text(refs, "panel_room") == room_id)
            && !main_participant_id.is_empty()
            && text(refs, "panel_room_parent") == main_participant_id
    };
    room_attempts.values().any(check) || array(&attempts["active"]).iter().any(check)
}

#[allow(clippy::too_many_arguments)]
fn project_main(
    repo: &Path,
    room: &Value,
    attempts: &Value,
    room_attempts: &serde_json::Map<String, Value>,
    store: &Store,
    panel_main: Option<&Value>,
    activity: &[Value],
    now_epoch: i64,
) -> Value {
    let external_running_count = activity
        .iter()
        .filter(|row| row["origin"] == "external" && is_active_peer_state(&text(row, "state")))
        .count();
    let has_panel_peer_work = activity
        .iter()
        .any(|row| row["origin"] == "panel" && is_active_peer_state(&text(row, "state")));
    let external_work_active = external_running_count > 0;
    let recorded_caller = activity
        .iter()
        .filter(|row| row["origin"] == "external" && is_active_peer_state(&text(row, "state")))
        .filter_map(|row| row["caller"].as_str().filter(|s| !s.is_empty()))
        .next()
        .map(normalize_provider);

    let Some(main_row) = panel_main else {
        if external_work_active {
            let ext_caller =
                recorded_caller.or_else(|| detect_external_hook_caller(repo, None, now_epoch));
            let (active_main, summary) = if let Some(ext) = ext_caller {
                (
                    ext.to_owned(),
                    format!(
                        "External Main ({}) · no panel Main",
                        provider_display_name(ext)
                    ),
                )
            } else {
                (
                    "external".to_owned(),
                    "No panel Main · work from external Main".to_owned(),
                )
            };
            return json!({
                "mode": "external",
                "active_main": active_main,
                "panel_main_state": "none",
                "panel_main_provider": Value::Null,
                "panel_main_attempt": Value::Null,
                "hook_state": Value::Null,
                "external_work_active": true,
                "external_running_count": external_running_count,
                "summary": summary,
            });
        }
        return json!({
            "mode": "none",
            "active_main": Value::Null,
            "panel_main_state": "none",
            "panel_main_provider": Value::Null,
            "panel_main_attempt": Value::Null,
            "hook_state": Value::Null,
            "external_work_active": false,
            "external_running_count": 0,
            "summary": "No active Main",
        });
    };

    let room_id = text(room, "id");
    let attempt_id = text(main_row, "attempt_id");
    let participant = main_participant(main_row);
    let raw_provider = text(main_row, "provider");
    let provider = normalize_provider(&raw_provider);
    let provider_name = provider_display_name(provider);
    let raw_state = text(main_row, "state");
    let (hook_state, hook_updated_at) =
        read_hook_activity_state(store, &room_id, participant, &attempt_id);
    let has_children =
        has_live_bound_room_children(room, attempts, room_attempts, &attempt_id, participant);
    let label = [
        text(main_row, "title"),
        text(&main_row["refs"], "panel_label"),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_default();
    let unrecorded_task = label == UNRECORDED_NATIVE_TASK;
    let has_tokens = main_row["usage_reports"]["tokens"]
        .as_u64()
        .is_some_and(|n| n > 0)
        && main_row["usage"]["tokens"].as_u64().is_some_and(|t| t > 0);
    let has_task_id = !text(main_row, "task_id").is_empty();

    let panel_main_idle = !has_children
        && !has_panel_peer_work
        && (hook_state.as_deref() == Some("idle")
            || (hook_state.is_none() && unrecorded_task && !has_tokens && !has_task_id));

    if panel_main_idle {
        let idle_at = if hook_state.as_deref() == Some("idle") {
            hook_updated_at
        } else {
            parse_epoch(&text(main_row, "updated_at"))
                .or_else(|| parse_epoch(&text(main_row, "created_at")))
                .filter(|&ts| ts > 0)
                .map(|ts| ts as f64)
        };
        if external_work_active {
            let ext_caller = recorded_caller
                .or_else(|| detect_external_hook_caller(repo, Some(provider), now_epoch));
            let (active_main, summary) = if let Some(ext) = ext_caller {
                (
                    ext.to_owned(),
                    format!(
                        "External Main ({}) · panel Main ({provider_name}) idle",
                        provider_display_name(ext)
                    ),
                )
            } else {
                (
                    "external".to_owned(),
                    format!("Panel Main ({provider_name}) idle · work from external Main"),
                )
            };
            json!({
                "mode": "external",
                "active_main": active_main,
                "panel_main_state": "idle",
                "panel_main_provider": provider,
                "panel_main_attempt": attempt_id,
                "hook_state": hook_state,
                "idle_at": idle_at,
                "external_work_active": true,
                "external_running_count": external_running_count,
                "summary": summary,
            })
        } else {
            json!({
                "mode": "panel_idle",
                "active_main": Value::Null,
                "panel_main_state": "idle",
                "panel_main_provider": provider,
                "panel_main_attempt": attempt_id,
                "hook_state": hook_state,
                "idle_at": idle_at,
                "external_work_active": false,
                "external_running_count": 0,
                "summary": format!("Panel Main ({provider_name}) idle"),
            })
        }
    } else {
        let summary = if external_work_active {
            format!(
                "Panel Main ({provider_name}) {raw_state} · {external_running_count} external peer run(s)"
            )
        } else {
            format!("Panel Main ({provider_name}) {raw_state}")
        };
        json!({
            "mode": "panel",
            "active_main": provider,
            "panel_main_state": raw_state,
            "panel_main_provider": provider,
            "panel_main_attempt": attempt_id,
            "hook_state": hook_state,
            "external_work_active": external_work_active,
            "external_running_count": external_running_count,
            "summary": summary,
        })
    }
}

pub fn project(
    repo: &Path,
    room: &Value,
    attempts: &Value,
    room_attempts: &serde_json::Map<String, Value>,
    store: &Store,
    stop: &AtomicBool,
) -> Value {
    let now_sys = SystemTime::now();
    let now_epoch = now_sys
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (_, scratch_peers) = crate::scratch_activity::observe_with_peer_runs(repo, stop);
    project_with_scratch(
        repo,
        room,
        attempts,
        room_attempts,
        store,
        &scratch_peers,
        now_epoch,
        now_sys,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn project_with_scratch(
    repo: &Path,
    room: &Value,
    attempts: &Value,
    room_attempts: &serde_json::Map<String, Value>,
    store: &Store,
    scratch_peers: &[ScratchPeerRun],
    now_epoch: i64,
    now_sys: SystemTime,
) -> Value {
    let room_id = text(room, "id");
    let panel_main = select_live_panel_main(room, attempts, room_attempts);
    let panel_main_attempt = panel_main
        .map(|row| text(row, "attempt_id"))
        .unwrap_or_default();
    let default_worktree = repo_worktree_label(repo);
    let mcp_ops = list_peer_operations(repo, MAX_ACTIVITY_ROWS, now_sys);
    let mut seen_attempts = BTreeSet::new();
    let mut mcp_index_by_op = BTreeMap::new();
    let mut activity = Vec::new();

    for rec in &mcp_ops {
        if let Some(aid) = &rec.attempt_id
            && !aid.is_empty()
        {
            seen_attempts.insert(aid.clone());
        }
        let idx = activity.len();
        mcp_index_by_op.insert(rec.operation.clone(), idx);
        activity.push(peer_operation_row(
            rec,
            &default_worktree,
            &panel_main_attempt,
            now_epoch,
        ));
    }

    for key in ["active", "recent"] {
        let rows = if attempts.is_array() {
            if key == "active" {
                array(attempts)
            } else {
                &[]
            }
        } else {
            array(&attempts[key])
        };
        for attempt in rows {
            let aid = text(attempt, "attempt_id");
            if aid.is_empty() {
                continue;
            }
            if let Some(mcp_op) = attempt_mcp_operation(attempt)
                && let Some(&idx) = mcp_index_by_op.get(&mcp_op)
            {
                seen_attempts.insert(aid.clone());
                let refs = &attempt["refs"];
                let rooms = [text(refs, "panel_room_id"), text(refs, "panel_room")];
                let foreign_room =
                    !room_id.is_empty() && rooms.iter().any(|r| !r.is_empty() && *r != room_id);
                if !foreign_room {
                    let row = &mut activity[idx];
                    if text(row, "attempt_id").is_empty() {
                        row["attempt_id"] = json!(aid);
                    }
                    let raw_provider = text(attempt, "provider");
                    if row["provider"] == "unknown" && !raw_provider.is_empty() {
                        let normalized = normalize_provider(&raw_provider);
                        row["provider"] = json!(normalized);
                        row["targets"] = json!([normalized]);
                    }
                    if text(row, "model").is_empty() {
                        let model = text(refs, "panel_model");
                        if !model.is_empty() {
                            row["model"] = json!(model);
                        }
                    }
                    let parent_attempt = text(attempt, "parent_attempt_id");
                    let bound_to_panel = (!panel_main_attempt.is_empty()
                        && parent_attempt == panel_main_attempt)
                        || (!room_id.is_empty() && rooms.contains(&room_id))
                        || (room_id.is_empty() && rooms.iter().any(|r| !r.is_empty()));
                    if bound_to_panel {
                        row["origin"] = json!("panel");
                    }
                    if row["caller"].is_null()
                        && let Some(caller) = [
                            text(refs, "caller_provider"),
                            text(refs, "main_provider"),
                            text(refs, "panel_owner"),
                        ]
                        .into_iter()
                        .find(|s| !s.is_empty())
                        .and_then(|s| safe_worktree_name(&s))
                    {
                        row["caller"] = json!(caller);
                    }
                }
                continue;
            }
            if !seen_attempts.insert(aid) {
                continue;
            }
            if let Some(row) = lifecycle_attempt_row(
                attempt,
                &room_id,
                &panel_main_attempt,
                &default_worktree,
                now_epoch,
            ) {
                activity.push(row);
            }
        }
    }

    for run in scratch_peers {
        activity.push(scratch_peer_row(run, now_epoch));
    }

    activity.sort_by(|a, b| {
        let a_live = is_live_or_stalled(&text(a, "state"));
        let b_live = is_live_or_stalled(&text(b, "state"));
        b_live
            .cmp(&a_live)
            .then_with(|| text(b, "started_at").cmp(&text(a, "started_at")))
            .then_with(|| text(a, "id").cmp(&text(b, "id")))
    });
    activity.truncate(MAX_ACTIVITY_ROWS);

    let main = project_main(
        repo,
        room,
        attempts,
        room_attempts,
        store,
        panel_main,
        &activity,
        now_epoch,
    );
    json!({
        "schema": 1,
        "main": main,
        "activity": activity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{CommandOutput, ExecutionRequest, NativeCommand, PeerExecutor, Services};
    use oms_command_operations::peer_operations::RUN_ROOT;
    use std::ffi::OsString;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(1);

    struct TempRepo(PathBuf);
    impl TempRepo {
        fn new(label: &str) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("oms-board-proj-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&dir)
                .unwrap();
            let canonical = std::fs::canonicalize(&dir).unwrap_or(dir);
            Self(canonical)
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct StubServices {
        attempts: Value,
        room: Value,
    }
    impl PeerExecutor for StubServices {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> crate::Result<CommandOutput> {
            Err(crate::Error::new("no subprocesses in board unit test"))
        }
    }
    impl Services for StubServices {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            _: &[OsString],
            _: &AtomicBool,
        ) -> crate::Result<CommandOutput> {
            let payload = match command {
                NativeCommand::Events => &self.attempts,
                NativeCommand::Room => &self.room,
                _ => return Err(crate::Error::new("unexpected command")),
            };
            Ok(CommandOutput {
                stdout: serde_json::to_vec(payload).unwrap(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            })
        }
    }

    #[test]
    fn running_peer_delegate_without_room_or_panel_main_projects_activity_and_external_main() {
        let repo = TempRepo::new("no-room-peer-delegate");
        let stop = AtomicBool::new(false);
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let started_epoch = now_epoch.saturating_sub(42);
        let started_iso = chrono::DateTime::<chrono::Utc>::from_timestamp(started_epoch, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let compact = chrono::DateTime::<chrono::Utc>::from_timestamp(started_epoch, 0)
            .unwrap()
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let op = format!("peer-delegate-{compact}-a1b2c3d4");
        let run_dir = repo.0.join(RUN_ROOT).join(&op);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        std::fs::write(run_dir.join("prompt.txt"), "Implement issue 465\n").unwrap();
        std::fs::write(run_dir.join("pid"), format!("{}\n", std::process::id())).unwrap();
        std::fs::write(
            run_dir.join("meta.json"),
            json!({
                "operation": op,
                "kind": "peer-delegate",
                "targets": ["codex:model=gpt-5"],
                "worktree": "wt-board-465",
                "started_at": started_iso,
                "title": "Project external Main activity"
            })
            .to_string(),
        )
        .unwrap();

        let services = StubServices {
            attempts: json!([]),
            room: json!({}),
        };
        let snapshot = crate::state::snapshot(&repo.0, None, &services, &stop).unwrap();
        let board = &snapshot.report["board"];
        let activity = board["activity"].as_array().expect("activity array");
        assert_eq!(activity.len(), 1, "{board}");
        let row = &activity[0];
        assert_eq!(row["tool"], "peer-delegate");
        assert_eq!(row["provider"], "codex");
        assert_eq!(row["worktree"], "wt-board-465");
        assert_eq!(row["state"], "running");
        assert!(
            row["elapsed_seconds"].as_u64().is_some_and(|n| n >= 40),
            "expected elapsed_seconds >= 40: {row}"
        );
        assert!(
            !text(row, "elapsed").is_empty(),
            "expected formatted elapsed time: {row}"
        );

        let main = &board["main"];
        assert_eq!(main["mode"], "external");
        assert_eq!(main["panel_main_state"], "none");
        assert_eq!(main["external_work_active"], true);
        assert_eq!(main["summary"], "No panel Main · work from external Main");
        assert_ne!(main["active_main"], "codex");

        for bytes in [
            include_bytes!("board.rs").as_slice(),
            include_bytes!("../../oms-command-operations/src/peer_operations.rs").as_slice(),
        ] {
            assert!(oms_scrub::scan(bytes, &[]).unwrap().is_empty());
        }
    }

    #[test]
    fn idle_panel_main_is_not_claimed_as_working_while_external_peer_runs_execute() {
        let repo = TempRepo::new("idle-panel-main");
        let now_sys = SystemTime::now();
        let now_epoch = now_sys.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let started = chrono::DateTime::<chrono::Utc>::from_timestamp(now_epoch - 95, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        // Dead PID without status file -> shared reader derives "stalled".
        let op = "peer-review-20261125T120000Z-deadbeef";
        let run_dir = repo.0.join(RUN_ROOT).join(op);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        std::fs::write(run_dir.join("prompt.txt"), "Gate review\n").unwrap();
        // Spawn and reap a quick process so its PID is guaranteed gone (ESRCH) or use max unused check:
        let mut exited = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = exited.id();
        let _ = exited.wait();
        std::fs::write(run_dir.join("pid"), format!("{gone_pid}\n")).unwrap();
        std::fs::write(
            run_dir.join("meta.json"),
            json!({
                "operation": op,
                "kind": "peer-review",
                "targets": ["claude"],
                "worktree": "wt-review-gate",
                "started_at": started,
            })
            .to_string(),
        )
        .unwrap();

        let idle_main = json!({
            "attempt_id": "att_idle_codex",
            "tool": "panel-main",
            "provider": "codex",
            "state": "working",
            "terminal": false,
            "created_at": started,
            "updated_at": started,
            "refs": {
                "panel_role": "main",
                "panel_room_id": "room-dogfood",
                "panel_label": UNRECORDED_NATIVE_TASK,
            }
        });
        let external_delegate = json!({
            "attempt_id": "att_ext_delegate",
            "tool": "peer-delegate",
            "provider": "claude",
            "state": "working",
            "terminal": false,
            "created_at": started,
            "updated_at": started,
            "refs": {
                "worktree": "wt-ext-1",
                "panel_label": "Fix board projection",
                "caller_provider": "claude",
            }
        });
        let room = json!({
            "id": "room-dogfood",
            "closed": false,
            "owner": "att_idle_codex",
            "main": "att_idle_codex",
            "participants": [{
                "participant": "att_idle_codex",
                "attempt": "att_idle_codex",
                "role": "main",
                "provider": "codex",
                "state": "working",
                "joined": true
            }]
        });
        let mut room_attempts = serde_json::Map::new();
        room_attempts.insert("att_idle_codex".into(), idle_main.clone());
        let scratch_peers = vec![ScratchPeerRun {
            id: "abcd1234abcd1234abcd1234abcd1234".into(),
            worktree: "wt-998877665544".into(),
            provider: Some("antigravity"),
            tool: "peer-review",
            state: "working".into(),
            created_at: started.clone(),
            updated_at: started,
        }];
        let store = Store::open(&repo.0).unwrap();
        let board = project_with_scratch(
            &repo.0,
            &room,
            &json!({"active": [idle_main, external_delegate], "recent": []}),
            &room_attempts,
            &store,
            &scratch_peers,
            now_epoch,
            now_sys,
        );

        assert_eq!(board["main"]["mode"], "external");
        assert_eq!(board["main"]["panel_main_state"], "idle");
        assert_eq!(board["main"]["active_main"], "claude");
        assert_eq!(
            board["main"]["summary"],
            "External Main (Claude) · panel Main (Codex) idle"
        );

        let rows = board["activity"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.iter().any(|r| {
            r["provider"] == "claude"
                && r["worktree"] == "wt-ext-1"
                && r["state"] == "working"
                && r["elapsed"] == "1m 35s"
        }));
        assert!(rows.iter().any(|r| {
            r["provider"] == "antigravity"
                && r["worktree"] == "wt-998877665544"
                && r["tool"] == "peer-review"
                && r["state"] == "working"
                && r["elapsed"] == "1m 35s"
        }));
        assert!(rows.iter().any(|r| {
            r["worktree"] == "wt-review-gate"
                && r["state"] == "stalled"
                && r["operation_status"] == "stalled"
        }));

        // Without explicit caller_provider on attempts, an active external Claude session
        // recorded in `.oms/hooks/events.jsonl` still identifies Claude as the external Main;
        // whereas peer-delegate started by the panel Main itself keeps mode == "panel".
        let hooks_dir = repo.0.join(".oms/hooks");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&hooks_dir)
            .unwrap();
        let hook_ts = chrono::DateTime::<chrono::Utc>::from_timestamp(now_epoch - 10, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let started_again = chrono::DateTime::<chrono::Utc>::from_timestamp(now_epoch - 50, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        std::fs::write(
            hooks_dir.join("events.jsonl"),
            format!(
                "{}\n",
                json!({
                    "schema": 1,
                    "ts": hook_ts,
                    "agent": "claude",
                    "hook": "PostToolUse",
                    "session": "sess-ext-claude"
                })
            ),
        )
        .unwrap();
        let unattributed_ext = json!({
            "attempt_id": "att_ext_unattributed",
            "tool": "peer-delegate",
            "provider": "codex",
            "state": "working",
            "terminal": false,
            "created_at": started_again,
            "updated_at": started_again,
            "refs": {
                "worktree": "wt-ext-2",
                "panel_label": "Unattributed external delegate"
            }
        });
        let hook_board = project_with_scratch(
            &repo.0,
            &room,
            &json!({"active": [room_attempts["att_idle_codex"].clone(), unattributed_ext], "recent": []}),
            &room_attempts,
            &store,
            &[],
            now_epoch,
            now_sys,
        );
        assert_eq!(hook_board["main"]["mode"], "external");
        assert_eq!(hook_board["main"]["active_main"], "claude");
        assert_eq!(
            hook_board["main"]["summary"],
            "External Main (Claude) · panel Main (Codex) idle"
        );

        let panel_owned_delegate = json!({
            "attempt_id": "att_panel_delegate",
            "parent_attempt_id": "att_idle_codex",
            "tool": "peer-delegate",
            "provider": "claude",
            "state": "working",
            "terminal": false,
            "created_at": started_again,
            "updated_at": started_again,
            "refs": {
                "panel_room_id": "room-dogfood",
                "worktree": "wt-panel-owned",
                "panel_label": "Panel-owned delegate"
            }
        });
        let panel_board = project_with_scratch(
            &repo.0,
            &room,
            &json!({"active": [room_attempts["att_idle_codex"].clone(), panel_owned_delegate], "recent": []}),
            &room_attempts,
            &store,
            &[],
            now_epoch,
            now_sys,
        );
        assert_eq!(panel_board["main"]["mode"], "panel");
        assert_eq!(panel_board["main"]["active_main"], "codex");
        assert!(
            panel_board["activity"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["worktree"] == "wt-panel-owned" && r["origin"] == "panel")
        );
    }

    #[test]
    fn mcp_consultation_deduplicates_with_downstream_lifecycle_attempts() {
        let repo = TempRepo::new("mcp-consult-dedup");
        let now_sys = SystemTime::now();
        let now_epoch = now_sys.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let started_epoch = now_epoch.saturating_sub(25);
        let started_iso = chrono::DateTime::<chrono::Utc>::from_timestamp(started_epoch, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let compact = chrono::DateTime::<chrono::Utc>::from_timestamp(started_epoch, 0)
            .unwrap()
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let op = format!("consult-{compact}-aabbccdd");
        let run_dir = repo.0.join(RUN_ROOT).join(&op);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        std::fs::write(run_dir.join("prompt.txt"), "Check architecture\n").unwrap();
        std::fs::write(run_dir.join("pid"), format!("{}\n", std::process::id())).unwrap();
        // Matches oms-mcp-server/src/peer.rs: meta.json records no attempt_id.
        std::fs::write(
            run_dir.join("meta.json"),
            json!({
                "operation": op,
                "kind": "consult",
                "targets": [],
                "started_at": started_iso,
                "answer_storage": "operation",
                "cancellation_supported": true,
                "title": "Check architecture"
            })
            .to_string(),
        )
        .unwrap();

        let downstream_attempt = json!({
            "attempt_id": "att_consult_seat1",
            "tool": "consult",
            "provider": "claude",
            "state": "working",
            "terminal": false,
            "created_at": started_iso,
            "updated_at": started_iso,
            "refs": {
                "mcp_operation": op,
                "artifact": format!(".oms/artifacts/mcp/{op}/answers/claude-call-1.md"),
                "panel_role": "advisor",
                "panel_model": "claude-sonnet-4-6",
                "panel_label": "consult (claude)"
            }
        });
        let store = Store::open(&repo.0).unwrap();
        let board = project_with_scratch(
            &repo.0,
            &json!({}),
            &json!({"active": [downstream_attempt], "recent": []}),
            &serde_json::Map::new(),
            &store,
            &[],
            now_epoch,
            now_sys,
        );
        let activity = board["activity"].as_array().unwrap();
        assert_eq!(activity.len(), 1, "{activity:?}");
        assert_eq!(activity[0]["id"], format!("mcp:{op}"));
        assert_eq!(activity[0]["attempt_id"], "att_consult_seat1");
        assert_eq!(activity[0]["provider"], "claude");
        assert_eq!(activity[0]["model"], "claude-sonnet-4-6");
        assert_eq!(board["main"]["external_running_count"], 1);
    }

    #[test]
    fn finished_peer_operations_freeze_elapsed_time_and_omit_unknown_span() {
        use rustix::fs::{self as rfs, AtFlags, Timespec, Timestamps, utimensat};

        let repo = TempRepo::new("finished-elapsed");
        let start_epoch = 1_764_072_000i64;
        let finish_epoch = start_epoch + 42;
        let started_iso = chrono::DateTime::<chrono::Utc>::from_timestamp(start_epoch, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let op = "advise-20251125T120000Z-1122aabb";
        let run_dir = repo.0.join(RUN_ROOT).join(op);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&run_dir)
            .unwrap();
        std::fs::write(run_dir.join("prompt.txt"), "Advise\n").unwrap();
        std::fs::write(
            run_dir.join("meta.json"),
            json!({
                "operation": op,
                "kind": "advise",
                "targets": ["claude"],
                "started_at": started_iso,
            })
            .to_string(),
        )
        .unwrap();
        let status_path = run_dir.join("status");
        std::fs::write(&status_path, "0\n").unwrap();
        let start_ts = Timespec {
            tv_sec: start_epoch as _,
            tv_nsec: 0,
        };
        let finish_ts = Timespec {
            tv_sec: finish_epoch as _,
            tv_nsec: 0,
        };
        utimensat(
            rfs::CWD,
            run_dir.join("prompt.txt"),
            &Timestamps {
                last_access: start_ts,
                last_modification: start_ts,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();
        utimensat(
            rfs::CWD,
            &status_path,
            &Timestamps {
                last_access: finish_ts,
                last_modification: finish_ts,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();

        // Second terminal record where finish < start so duration cannot be determined.
        let unmeasurable = "consult-20251125T115900Z-ccddeeff";
        let bad_dir = repo.0.join(RUN_ROOT).join(unmeasurable);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&bad_dir)
            .unwrap();
        std::fs::write(
            bad_dir.join("meta.json"),
            json!({
                "operation": unmeasurable,
                "kind": "consult",
                "targets": ["codex"],
                "started_at": started_iso,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(bad_dir.join("status"), "0\n").unwrap();
        let before_start_ts = Timespec {
            tv_sec: (start_epoch - 10) as _,
            tv_nsec: 0,
        };
        utimensat(
            rfs::CWD,
            bad_dir.join("status"),
            &Timestamps {
                last_access: before_start_ts,
                last_modification: before_start_ts,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();

        let store = Store::open(&repo.0).unwrap();
        for later in [start_epoch + 300, start_epoch + 3600] {
            let later_sys = UNIX_EPOCH + std::time::Duration::from_secs(later as u64);
            let board = project_with_scratch(
                &repo.0,
                &json!({}),
                &json!({"active": [], "recent": []}),
                &serde_json::Map::new(),
                &store,
                &[],
                later,
                later_sys,
            );
            let rows = board["activity"].as_array().unwrap();
            let done_row = rows.iter().find(|r| r["operation"] == op).unwrap();
            assert_eq!(done_row["state"], "done");
            assert_eq!(done_row["elapsed_seconds"], 42);
            assert_eq!(done_row["elapsed"], "42s");

            let unknown_row = rows
                .iter()
                .find(|r| r["operation"] == unmeasurable)
                .unwrap();
            assert_eq!(unknown_row["state"], "done");
            assert!(unknown_row["elapsed_seconds"].is_null(), "{unknown_row}");
            assert_eq!(unknown_row["elapsed"], "");
        }
    }
}
