//! Actual local lifecycle and packet producers. These functions never execute
//! commands stored in packets, reports, plans or peer messages.
use crate::{
    Result, V,
    capture::{self, CaptureIndex},
    checkpoint, digest, now, obj, storage,
};
use oms_command_operations::{self as events, StartRequest};
use std::{io::Write, os::unix::ffi::OsStrExt, path::Path, sync::atomic::AtomicBool};

/// Emit the attempt identity before its transitions, preserving recovery when
/// a later lifecycle write fails. The output is flushed at that boundary.
pub fn begin(
    repo: &Path,
    tool: &str,
    input: Option<&Path>,
    output: &mut dyn Write,
    interrupted: &AtomicBool,
) -> Result<String> {
    let request = request(tool, input.filter(|path| !path.as_os_str().is_empty()))?;
    checkpoint(interrupted)?;
    let attempt = events::create_attempt(repo, &request, interrupted)?;
    writeln!(output, "{attempt}")
        .and_then(|()| output.flush())
        .map_err(|e| e.to_string())?;
    transition(repo, &attempt, "starting", interrupted)?;
    transition(repo, &attempt, "working", interrupted)?;
    Ok(attempt)
}
pub(crate) fn request(tool: &str, input: Option<&Path>) -> Result<StartRequest> {
    if !matches!(tool, "agent-task" | "patch-admit") {
        return Err("unsupported reference writer".into());
    }
    let input = input
        .map(storage::absolute)
        .transpose()?
        .map(|v| digest(v.as_os_str().as_bytes()))
        .unwrap_or_default();
    Ok(StartRequest {
        provider: "local".into(),
        tool: tool.into(),
        refs: obj([
            ("reference_writer", "landing-reference-v1".into()),
            ("reference_input_sha256", input.into()),
        ]),
        ..Default::default()
    })
}
pub fn transition(repo: &Path, attempt: &str, state: &str, interrupted: &AtomicBool) -> Result<V> {
    let event = events::new_event(
        attempt,
        0usize.into(),
        "attempt.state_changed",
        obj([
            ("from_state", V::Null),
            ("to_state", state.into()),
            ("reason_code", "reference-publication".into()),
            (
                "actor",
                obj([
                    ("kind", "system".into()),
                    ("name", "landing-reference".into()),
                ]),
            ),
        ]),
    )?;
    events::append_lifecycle(repo, event, interrupted)
}
/// A failed publication remains blocked/nonterminal. The terminal failed state
/// is reserved for a proven no-write refusal; a crash never calls this leg.
pub fn finish(repo: &Path, attempt: &str, result: &str, interrupted: &AtomicBool) -> Result<()> {
    let (rows, attempts) = events::load_projection(repo)?;
    let current = attempts
        .iter()
        .find(|v| v.get("attempt_id").text() == Some(attempt))
        .ok_or("not an owned local reference operation")?;
    let created = rows
        .iter()
        .filter(|v| {
            v.get("attempt_id").text() == Some(attempt)
                && v.get("event_type").text() == Some("attempt.created")
        })
        .collect::<Vec<_>>();
    if created.len() != 1
        || current.get("provider").text() != Some("local")
        || !matches!(
            current.get("tool").text(),
            Some("agent-task" | "patch-admit")
        )
        || created[0].get("refs").get("reference_writer").text() != Some("landing-reference-v1")
        || current.get("parent_attempt_id").truth()
    {
        return Err("not an owned local reference operation".into());
    }
    if matches!(current.get("terminal"), V::Bool(true)) {
        return Ok(());
    }
    let states: &[&str] = match result {
        "refuse-no-write" => &["failed"],
        "0" => &["verifying", "review", "done"],
        _ => &["blocked"],
    };
    for state in states {
        transition(repo, attempt, state, interrupted)?;
    }
    Ok(())
}

/// Internal transaction leg. The caller must hold the canonical packet lock
/// and an actual live reference operation throughout validation/publication.
/// The note and its raw-input receipt publish together in one replacement.
#[allow(clippy::too_many_arguments)]
pub fn append_deduplicated_unlocked(
    index: &CaptureIndex,
    attempt: &str,
    target: &Path,
    section: &str,
    agent: &[u8],
    note: &Path,
    key: &[u8],
    source_session: &[u8],
    interrupted: &AtomicBool,
) -> Result<()> {
    checkpoint(interrupted)?;
    let (rows, attempts) = events::load_projection(&index.repo)?;
    capture::owned_created(&rows, &attempts, attempt, "agent-task", target)
        .map_err(|_| "task append requires its actual local reference operation")?;
    let current = attempts
        .iter()
        .find(|v| v.get("attempt_id").text() == Some(attempt))
        .ok_or("task append requires its actual local reference operation")?;
    if !matches!(current.get("terminal"), V::Bool(false)) {
        return Err("task append requires its actual local reference operation".into());
    }
    let target = storage::absolute(target)?;
    let parent = target.parent().ok_or("task packet has no parent")?;
    let (raw, before_info) = if capture::exists(&target)? {
        let (raw, info) = storage::regular(&target, storage::MAX_STREAM, 1)?;
        (raw, Some(info))
    } else {
        (Vec::new(), None)
    };
    let (content, note_info) = storage::regular(note, storage::MAX_STREAM, 1)?;
    if capture::dedupe_state(&raw, &content, key)? {
        index.check()?;
        return Ok(());
    }
    let stamp = now();
    let suffix =
        u16::from_str_radix(&storage::random_hex(2)?, 16).map_err(|e| e.to_string())? % 32768;
    let id = format!(
        "task-{}-{}-{suffix}",
        stamp.replace(['-', ':'], ""),
        std::process::id()
    );
    let initialized = oms_command_task::packet::ensure(raw.clone(), &stamp, &id, source_session);
    let section = section.strip_prefix("## ").unwrap_or(section);
    let rendered = oms_command_task::packet::append(
        &initialized,
        section,
        agent,
        &content,
        &stamp,
        cap("OMS_AGENT_TASK_NOTE_CHARS", 300),
        cap("OMS_AGENT_TASK_MAX_CURRENT_BULLETS", 100),
    );
    let candidate = oms_command_task::packet::add_receipt(&rendered, &content, key)
        .map_err(|e| e.to_string())?;
    let (after_note, after_note_info) = storage::regular(note, storage::MAX_STREAM, 1)?;
    if content != after_note || !storage::generation(&note_info, &after_note_info) {
        return Err("task note changed during append".into());
    }
    let current_info = if capture::exists(&target)? {
        Some(storage::regular(&target, storage::MAX_STREAM, 1)?)
    } else {
        None
    };
    if current_info
        .as_ref()
        .map(|v| v.0.as_slice())
        .unwrap_or_default()
        != raw
        || match (&before_info, &current_info) {
            (Some(a), Some((_, b))) => !storage::generation(a, b),
            (None, None) => false,
            _ => true,
        }
    {
        return Err("task packet changed during append".into());
    }
    index.validate_write(
        &target,
        "agent_task_append_bullet_unlocked",
        &[
            target.as_os_str().as_bytes().to_vec(),
            section.as_bytes().to_vec(),
            agent.to_vec(),
            note.as_os_str().as_bytes().to_vec(),
        ],
        &raw,
        &candidate,
        source_session,
    )?;
    checkpoint(interrupted)?;
    // A FILE-only packet may live outside the state checkout. Its parent is
    // its own publication root; managed pins still require a scanned target.
    if !parent.is_dir() {
        if let Ok(relative) = target.strip_prefix(&index.repo) {
            storage::ensure_parent(&index.repo, relative)?;
        } else {
            let mut ancestor = parent;
            while !capture::exists(ancestor)? {
                ancestor = ancestor
                    .parent()
                    .ok_or("task packet parent does not exist")?;
            }
            let relative = target
                .strip_prefix(ancestor)
                .map_err(|_| "task packet parent leaves namespace")?;
            storage::ensure_parent(ancestor, relative)?;
        }
    }
    storage::replace(
        parent,
        Path::new(target.file_name().ok_or("task packet has no name")?),
        &raw,
        &candidate,
        true,
        before_info.as_ref(),
    )
}
fn cap(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| {
            if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
                Some(v.parse().unwrap_or(usize::MAX))
            } else {
                None
            }
        })
        .unwrap_or(default)
}
