//! Adapters for peer-common, agent-call and peer-delegate. They share the CLI
//! writer and its reference protocol instead of writing task packets directly.
use crate::{Result, Services, cli, packet};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::Path,
    sync::atomic::AtomicBool,
};

/// Render peer-call budget reminders from a fresh packet. `changed_lines` is
/// a trusted native Git inspector's current add+delete count (or unavailable),
/// never a guess inferred from missing or unsupported Git refs. Inspection is
/// separate so callers do not execute configured Git filters for a reminder.
pub fn loop_warnings(raw: &[u8], changed_lines: Option<&[u8]>) -> Result<Vec<u8>> {
    if raw.is_empty() || crate::safe(raw, "packet").is_err() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    let attempts = packet::section_value(raw, "Loop State", "attempts");
    let max = packet::section_value(raw, "Loop State", "max_attempts");
    let number = |s: &[u8]| !s.is_empty() && s.iter().all(u8::is_ascii_digit);
    if number(&attempts)
        && number(&max)
        && packet::decimal_cmp(&max, b"0").is_gt()
        && !packet::decimal_cmp(&attempts, &max).is_lt()
    {
        crate::line(
            &mut output,
            &[b"warning: loop attempts exhausted: ", &attempts, b"/", &max],
        )?;
    }
    let failures = packet::section(raw, "Last Failure");
    let rows: Vec<_> = packet::lines(&failures)
        .into_iter()
        .filter_map(|l| {
            if !l.starts_with(b"- ") {
                return None;
            }
            let mut line = l;
            // awk's provenance-removal pattern only recognizes the first bracket.
            if let Some(open) = l[2..].iter().position(|b| *b == b'[').map(|i| i + 2)
                && let Some(close) = l[open + 1..]
                    .iter()
                    .position(|b| *b == b']')
                    .map(|i| i + open + 1)
                && l.get(close + 1) == Some(&b' ')
            {
                line = &l[close + 2..];
            }
            if line.is_empty() { None } else { Some(line) }
        })
        .collect();
    if rows.len() >= 3
        && rows[rows.len() - 3..]
            .iter()
            .all(|l| *l == rows[rows.len() - 1])
    {
        crate::line(
            &mut output,
            &[
                b"warning: repeated last failure detected (3x): ",
                rows[rows.len() - 1],
            ],
        )?;
    }
    let budget = packet::section_value(raw, "Loop State", "diff_budget_lines");
    if number(&budget)
        && packet::decimal_cmp(&budget, b"0").is_gt()
        && let Some(changed) = changed_lines.filter(|s| number(s))
        && packet::decimal_cmp(changed, &budget).is_gt()
    {
        crate::line(
            &mut output,
            &[
                b"warning: loop diff budget exceeded: ",
                changed,
                b"/",
                &budget,
                b" changed lines",
            ],
        )?;
    }
    Ok(output)
}

pub struct TaskOutcome<'a> {
    pub kind: &'a [u8],
    pub provider: &'a [u8],
    pub status: &'a [u8],
    pub artifact: &'a Path,
    pub patch: Option<&'a Path>,
    pub detail: &'a [u8],
}
/// Observe a real existing worker artifact in Current State. Missing artifact or
/// packet is a no-op; this never initializes a task for a completed worker.
/// Returns the writer's actual failure, letting the caller issue the existing
/// advisory warning without claiming a recorded outcome.
pub fn record_outcome(
    repo: &Path,
    event: TaskOutcome<'_>,
    services: &mut dyn Services,
    interrupted: &AtomicBool,
) -> Result<()> {
    let artifact = if event.artifact.is_absolute() {
        event.artifact.to_path_buf()
    } else {
        repo.join(event.artifact)
    };
    if !artifact.is_file() {
        return Ok(());
    }
    let root = crate::git::repository(repo, interrupted)?;
    let store = crate::storage::Store::new(&root, &root.join(".oms/task/current.md"))?;
    if store.read()?.is_none_or(|raw| raw.is_empty()) {
        return Ok(());
    }
    fn relative<'a>(repo: &Path, path: &'a Path) -> &'a [u8] {
        path.strip_prefix(repo)
            .unwrap_or_else(|_| Path::new(path.file_name().unwrap_or_default()))
            .as_os_str()
            .as_bytes()
    }
    let mut note = [
        event.kind,
        b" ",
        event.provider,
        b" exit=",
        event.status,
        b"; artifact=",
        relative(&root, &artifact),
    ]
    .concat();
    if let Some(patch) = event.patch {
        let patch = if patch.is_absolute() {
            patch.to_path_buf()
        } else {
            root.join(patch)
        };
        note.extend_from_slice(b"; patch=");
        note.extend_from_slice(relative(&root, &patch));
    }
    if !event.detail.is_empty() {
        note.extend_from_slice(b"; ");
        note.extend_from_slice(event.detail);
    }
    let args: Vec<OsString> = [
        OsStr::new("append"),
        OsStr::new("--agent"),
        OsStr::from_bytes(event.kind),
        OsStr::new("--text"),
        OsStr::from_bytes(&note),
    ]
    .into_iter()
    .map(OsStr::to_owned)
    .collect();
    let options = cli::parse(&args)?;
    crate::safe(&note, "note")?;
    let task = crate::Task {
        options,
        repo: root.clone(),
        state_repo: services
            .state_repository(&root, interrupted)?
            .canonicalize()
            .map_err(|e| crate::Error::io("resolve outcome state repository", e))?,
        display_path: store.path.clone(),
        lock_path: store.path.clone(),
        store,
        agent: event.kind.to_vec(),
        interrupted,
        signal: None,
        note_cap: crate::env_cap("OMS_AGENT_TASK_NOTE_CHARS", 300),
        current_cap: crate::env_cap("OMS_AGENT_TASK_MAX_CURRENT_BULLETS", 100),
    };
    task.operation(services, |reference| {
        let Some(raw) = task.store.read()?.filter(|raw| !raw.is_empty()) else {
            return Ok(());
        };
        let body = task.append(&task.initialize(Some(raw.clone())), "Current State", &note);
        task.publish(
            "agent_task_append_bullet_unlocked",
            Some(&raw),
            &body,
            reference,
        )
    })
}

/// Parent/worker guard for native internal callers, using the same environment
/// contract as the public command. No mutation or service invocation occurs.
pub fn parent_guard(args: &[OsString]) -> Result<()> {
    let options = cli::parse(args)?;
    cli::parent_guard(&options)
}

/// File-only reference ownership used by internal task helpers. An explicit
/// repository wins; otherwise a physical .oms evidence namespace wins over an
/// enclosing checkout, matching agent_task_reference_root.
pub fn reference_root(
    file: &Path,
    explicit_repo: Option<&Path>,
    services: &mut dyn Services,
    interrupted: &AtomicBool,
) -> Result<std::path::PathBuf> {
    if let Some(repo) = explicit_repo {
        let root = crate::git::repository(repo, interrupted)?;
        return services.state_repository(&root, interrupted);
    }
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let physical = parent.canonicalize().map_err(|e| {
        crate::Error::io("task file parent does not exist; supply its repository", e)
    })?;
    for scope in physical.ancestors() {
        if let Some(oms) = scope.parent()
            && oms.file_name() == Some(OsStr::new(".oms"))
            && ["task", "tasks", "plan", "artifacts"]
                .iter()
                .any(|name| scope.file_name() == Some(OsStr::new(name)))
        {
            let owner = oms
                .parent()
                .ok_or_else(|| crate::Error::new(1, "task packet repository does not exist"))?;
            let root = crate::git::repository(owner, interrupted)?;
            if root != owner {
                return Ok(owner.to_owned());
            }
            return services.state_repository(owner, interrupted);
        }
    }
    let root = crate::git::repository(&physical, interrupted)?;
    services.state_repository(&root, interrupted)
}
