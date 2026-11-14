#![forbid(unsafe_code)]

pub mod adapters;
pub mod cli;
pub mod cold;
pub mod config;
mod files;
mod labels;
pub mod notion;
pub mod render;
pub mod sanitize;
pub mod schema;
pub mod search;
pub mod store;
pub mod tz;
mod unicode;

use crate::adapters::build_source_payload;
use crate::cli::{JournalCommand, ShowMode, help_text, parse_command};
use crate::cold::seal_report_text;
use crate::config::{
    configure_notion, expand_user_path, notion_settings, purge_notion_config, set_journal_language,
    update_notion_exclusions,
};
use crate::notion::{NotionExporter, SyncOptions, sync_notion};
use crate::sanitize::pretty_json_bytes;
use crate::schema::{atomic_write_bytes, canonical_repo_root, project_id_path, work_journal_dir};
use crate::search::run_recorded_search;
use crate::store::{JournalStore, MemoryAppendFn};
pub use crate::store::{MemoryAppendFn as JournalMemoryAppendFn, MemoryAppendOutcome};
use crate::tz::current_utc_now;
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const CHILD_MUTATION_ERROR: &str = "error: a harness child cannot mutate parent-owned host or global state; return the request to the parent agent\n";

pub fn owns(_args: &[OsString]) -> bool {
    true
}

pub fn run(
    args: &[OsString],
    default_repo: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_source(args, default_repo, default_repo, interrupted)
}

pub fn run_with_source(
    args: &[OsString],
    cwd: &Path,
    source_root: &Path,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    run_with_services(args, cwd, source_root, None, interrupted)
}

/// Run the journal command with an optional in-process native memory append adapter.
///
/// When `distill` (without `--dry-run`) or `tick --autodistill` has lessons to promote,
/// `memory_append` is called with `(repo_root, "journal", lesson_text)` and returns
/// `Ok(MemoryAppendOutcome::Appended)` or `Ok(MemoryAppendOutcome::Refused(reason))`.
pub fn run_with_services(
    args: &[OsString],
    cwd: &Path,
    _source_root: &Path,
    memory_append: Option<MemoryAppendFn<'_>>,
    interrupted: &AtomicBool,
) -> (i32, Vec<u8>, Vec<u8>) {
    let cmd = match parse_command(args) {
        Ok(c) => c,
        Err(msg) => {
            let trimmed = if msg.ends_with('\n') {
                msg
            } else {
                format!("{msg}\n")
            };
            return (2, Vec::new(), trimmed.into_bytes());
        }
    };

    if matches!(cmd, JournalCommand::Help) {
        return (
            0,
            crate::cli::contextual_help(args).into_bytes(),
            Vec::new(),
        );
    }

    if is_harness_child()
        && matches!(
            cmd,
            JournalCommand::Configure { .. }
                | JournalCommand::Disconnect { .. }
                | JournalCommand::Sync { .. }
                | JournalCommand::Resolve { .. }
        )
    {
        return (2, Vec::new(), CHILD_MUTATION_ERROR.as_bytes().to_vec());
    }

    match execute_command(cmd, cwd, memory_append, interrupted) {
        Ok((code, out, err)) => (code, out, err),
        Err(err) => {
            let line = if err.ends_with('\n') {
                err
            } else {
                format!("error: work journal: {err}\n")
            };
            (1, Vec::new(), line.into_bytes())
        }
    }
}

fn is_harness_child() -> bool {
    std::env::var("OMS_HARNESS_CHILD").is_ok_and(|v| v == "1")
}

fn non_empty_trimmed_path(raw: Option<&Path>) -> Option<PathBuf> {
    raw.filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn resolve_cli_path(raw: &Path, cwd: &Path) -> PathBuf {
    let expanded = expand_user_path(raw);
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

fn resolve_repo(explicit: Option<&Path>, cwd: &Path) -> PathBuf {
    let candidate = match non_empty_trimmed_path(explicit) {
        Some(raw) => resolve_cli_path(&raw, cwd),
        None => cwd.to_path_buf(),
    };
    canonical_repo_root(&candidate)
}

fn state_lock_target(repo_root: &Path) -> PathBuf {
    work_journal_dir(repo_root).join("state")
}

fn sync_lock_target(repo_root: &Path) -> PathBuf {
    work_journal_dir(repo_root).join("notion-sync")
}

fn acquire_blocking_lock(target: &Path) -> Result<oms_lock::Guard, String> {
    let settings =
        oms_lock::Settings::from_env().map_err(|e| format!("lock settings error: {e}"))?;
    let lock =
        oms_lock::Lock::for_file(target, settings).map_err(|e| format!("lock setup error: {e}"))?;
    lock.acquire(None)
        .map_err(|e| format!("failed acquiring lock {}: {e}", target.display()))
}

enum TryLockResult {
    Acquired(oms_lock::Guard),
    Busy,
}

fn try_acquire_lock(target: &Path) -> Result<TryLockResult, String> {
    let settings =
        oms_lock::Settings::from_env().map_err(|e| format!("lock settings error: {e}"))?;
    let lock =
        oms_lock::Lock::for_file(target, settings).map_err(|e| format!("lock setup error: {e}"))?;
    match lock.try_acquire() {
        Ok(guard) => Ok(TryLockResult::Acquired(guard)),
        Err(e) if e.exit_code() == 75 => Ok(TryLockResult::Busy),
        Err(e) => Err(format!("failed acquiring lock {}: {e}", target.display())),
    }
}

fn execute_command(
    cmd: JournalCommand,
    cwd: &Path,
    memory_append: Option<MemoryAppendFn<'_>>,
    interrupted: &AtomicBool,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let now = current_utc_now();

    match cmd {
        JournalCommand::Help => Ok((0, help_text().as_bytes().to_vec(), Vec::new())),

        JournalCommand::Configure {
            data_source_id,
            discover,
            exclude_repos,
            include_repos,
            lang,
            validate,
        } => {
            if let Some(lang) = lang {
                let path = set_journal_language(&lang)?;
                return Ok((0,format!("journal language: {lang} ({})\nrun `oms journal rebuild` to re-render existing summaries\n",path.display()).into_bytes(),Vec::new()));
            }
            if !exclude_repos.is_empty() || !include_repos.is_empty() {
                let paths = |values: &[PathBuf]| {
                    values
                        .iter()
                        .map(|p| resolve_cli_path(p, cwd))
                        .collect::<Vec<_>>()
                };
                let (_, list) =
                    update_notion_exclusions(&paths(&exclude_repos), &paths(&include_repos))?;
                let output = if list.is_empty() {
                    "notion sync excludes no repos\n".into()
                } else {
                    format!(
                        "notion sync excludes {} repo(s):\n{}\n",
                        list.len(),
                        list.iter()
                            .map(|s| format!("  {s}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                };
                return Ok((0, output.into_bytes(), Vec::new()));
            }
            let mut settings = notion_settings()?;
            let target = if discover {
                let mut discovery = settings.clone();
                discovery.auth_value.clear();
                discovery.auth_mode = "ntn".into();
                discovery.transport = "ntn".into();
                if !crate::config::notion_auth_available(&discovery) {
                    return Err("Notion CLI is required; install ntn and run ntn login".into());
                }
                NotionExporter::from_settings(discovery, 8.0)?
                    .discover_work_journal_data_source(interrupted)?
            } else {
                data_source_id.unwrap_or_default()
            };
            settings.data_source_id = target.clone();
            settings.database_id.clear();
            if discover {
                settings.auth_mode = "ntn".into();
                settings.transport = "ntn".into();
            }
            for (field, value) in [
                (&mut settings.title_property, "Name"),
                (&mut settings.key_property, "Work Journal Key"),
                (&mut settings.hash_property, "Content Hash"),
                (&mut settings.project_property, "Project"),
                (&mut settings.kind_property, "Kind"),
                (&mut settings.period_property, "Period"),
                (&mut settings.blocker_property, "Has Blocker"),
                (&mut settings.sessions_property, "Sessions"),
                (&mut settings.commits_property, "Commits"),
                (&mut settings.verified_property, "Verified"),
            ] {
                *field = value.into();
            }
            if validate && crate::config::notion_auth_available(&settings) {
                NotionExporter::from_settings(settings.clone(), settings.budget_seconds)?
                    .validate_target(interrupted)?;
            }
            let path = configure_notion(&target, if discover { Some("ntn") } else { None })?;
            let settings = notion_settings()?;
            let mut output = format!("configured: {}\n", path.display());
            if crate::config::notion_auth_available(&settings) {
                let mode = if settings.auth_mode.is_empty() {
                    concat!("to", "ken")
                } else {
                    &settings.auth_mode
                };
                output.push_str(&format!("Notion access: {mode}; target schema validated\n"));
            } else {
                output.push_str(concat!(
                    "Notion access: missing (run ntn login or set OMS_WORK_JOURNAL_NOTION_",
                    "TOK",
                    "EN)\n"
                ));
            }
            Ok((0, output.into_bytes(), Vec::new()))
        }

        JournalCommand::Disconnect { managed } => {
            if !managed {
                return Err("disconnect requires --managed".into());
            }
            Ok((
                0,
                if purge_notion_config()? {
                    b"removed\n".to_vec()
                } else {
                    b"not configured\n".to_vec()
                },
                Vec::new(),
            ))
        }

        JournalCommand::Identity {
            repo,
            json,
            adopt_detected,
        } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_read(&repo_root, None)?;
            let (detected_id, detected_name) = crate::schema::detected_identity(&repo_root);
            let pinned = store.pinned_identity()?;
            let drift = pinned
                .as_ref()
                .is_some_and(|(id, name)| *id != detected_id || *name != detected_name);
            if adopt_detected {
                if pinned.is_some() && !drift {
                    return Ok((
                        0,
                        format!(
                            "identity already canonical: {} ({})\n",
                            store.project.name, store.project.id
                        )
                        .into_bytes(),
                        Vec::new(),
                    ));
                }
                let value = serde_json::json!({"schema_version":1,"project_id":detected_id,"project_name":detected_name});
                atomic_write_bytes(&project_id_path(&repo_root), &pretty_json_bytes(&value)?)?;
                crate::files::remove_owned_file(&store.root.join("sync/notion.json"))?;
                let rebuilt = JournalStore::open_for_read(&repo_root, None)?.rebuild()?;
                let old_id = pinned.as_ref().map(|(id, _)| id.as_str()).unwrap_or("-");
                let old_name = pinned
                    .as_ref()
                    .map(|(_, name)| name.as_str())
                    .unwrap_or("unpinned");
                return Ok((0,format!("adopted: {detected_name} ({detected_id}) — was {old_name} ({old_id}); rebuilt {} events, {} daily, {} weekly\nnext: oms journal sync --force\n",rebuilt.event_count,rebuilt.daily_periods.len(),rebuilt.weekly_periods.len()).into_bytes(),Vec::new()));
            }
            let row = serde_json::json!({"pinned_id":pinned.as_ref().map(|(id,_)|id),"pinned_name":pinned.as_ref().map(|(_,name)|name),"detected_id":detected_id,"detected_name":detected_name,"drift":drift});
            if json {
                return Ok((0, json_output(&row)?, Vec::new()));
            }
            let text = format!(
                "pinned:   {} ({})\ndetected: {detected_name} ({detected_id})\n{}\n",
                pinned
                    .as_ref()
                    .map(|(_, name)| name.as_str())
                    .unwrap_or("(none)"),
                pinned.as_ref().map(|(id, _)| id.as_str()).unwrap_or("-"),
                if drift {
                    "drift: yes — `oms journal identity --adopt-detected` re-pins, rebuilds, and resets the Notion mapping"
                } else {
                    "drift: no"
                }
            );
            Ok((0, text.into_bytes(), Vec::new()))
        }

        JournalCommand::Status { repo, json } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_read(&repo_root, None)?;
            let status = store.status_report()?;
            if json {
                let mut wire = crate::sanitize::to_oms_json(&status)?;
                if let oms_json::Value::Object(object) = &mut wire {
                    object.insert(
                        "repo".into(),
                        oms_json::Value::String(crate::sanitize::os_str_to_py_string(
                            repo_root.as_os_str(),
                        )),
                    );
                }
                return Ok((0, crate::sanitize::default_json_bytes(&wire)?, Vec::new()));
            }
            let project = &status["project"];
            let notion = &status["notion"];
            let drift = if project["drift"].as_bool() == Some(true) {
                format!(
                    " — detected {}; `oms journal identity --adopt-detected` merges",
                    project["detected_name"].as_str().unwrap_or("")
                )
            } else {
                String::new()
            };
            let text = format!(
                "\n- project: {} ({}){drift}\n- events: {} ({} bytes)\n- summaries: {} daily, {} weekly\n- notion: {}, credential {}, {} synced, {} pending, {} failed\n",
                project["name"].as_str().unwrap_or(""),
                project["id"].as_str().unwrap_or(""),
                status["event_count"],
                status["events_bytes"],
                status["daily_count"],
                status["weekly_count"],
                if notion["configured"].as_bool() == Some(true) {
                    "configured"
                } else {
                    "not configured"
                },
                if notion[concat!("cred", "ential_present")].as_bool() == Some(true) {
                    "present"
                } else {
                    "missing"
                },
                notion["synced"],
                notion["pending"],
                notion["failed"]
            );
            use std::os::unix::ffi::OsStrExt;
            let mut output = b"Work Journal\n- repo: ".to_vec();
            output.extend_from_slice(repo_root.as_os_str().as_bytes());
            output.extend_from_slice(text.as_bytes());
            Ok((0, output, Vec::new()))
        }

        JournalCommand::Observe(mut obs) => {
            let repo_root = resolve_repo(obs.repo.as_deref(), cwd);
            obs.inner.source_file = non_empty_trimmed_path(obs.inner.source_file.as_deref())
                .map(|p| resolve_cli_path(&p, cwd));
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let payload = build_source_payload(&repo_root, &obs.inner, now)?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            store.record_event(&payload, now)?;
            store.materialize()?;
            Ok((0, Vec::new(), Vec::new()))
        }

        JournalCommand::Tick {
            repo,
            local_only: _,
            digest,
            autodistill,
        } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            store.capture_head_commit(now)?;
            store.materialize()?;
            let mut out = String::new();
            if digest {
                if let Some(value) = store.prompt_digest(now)? {
                    out.push_str(&value);
                    out.push('\n');
                }
            }
            if autodistill {
                if let Ok(true) = store.distill_due_today(now) {
                    if let Ok(report) = store.distill(false, memory_append, now) {
                        let mut counts = Vec::new();
                        if report.promoted > 0 {
                            counts.push(format!("{} promoted", report.promoted));
                        }
                        if report.deduped > 0 {
                            counts.push(format!("{} already in shared memory", report.deduped));
                        }
                        if !report.skipped_refused.is_empty() {
                            counts.push(format!(
                                "{} refused by the memory writer",
                                report.skipped_refused.len()
                            ));
                        }
                        if report.dropped > 0 {
                            counts.push(format!("{} beyond the per-run cap", report.dropped));
                        }
                        if !counts.is_empty() {
                            out.push_str(&format!(
                                "[work-journal] distill: {}\n",
                                counts.join(", ")
                            ));
                        }
                    }
                }
            }
            if let Ok(signature) = std::env::var("OMS_JOURNAL_PROMPT_STAMP") {
                if !signature.is_empty() {
                    store.write_prompt_stamp(&signature, autodistill, now)?;
                }
            }
            Ok((0, out.into_bytes(), Vec::new()))
        }

        JournalCommand::Materialize { repo } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            store.materialize()?;
            Ok((0, Vec::new(), Vec::new()))
        }

        JournalCommand::Rebuild { repo } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            let report = store.rebuild()?;
            let msg = format!(
                "rebuilt: {} events, {} daily, {} weekly\n",
                report.event_count,
                report.daily_periods.len(),
                report.weekly_periods.len()
            );
            Ok((0, msg.into_bytes(), Vec::new()))
        }

        JournalCommand::Seal { repo, apply } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _sync_guard = acquire_blocking_lock(&sync_lock_target(&repo_root))?;
            let _state_guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_read(&repo_root, None)?;
            let count = store.seal(apply, now)?;
            let msg = seal_report_text(!apply, count);
            Ok((0, msg.into_bytes(), Vec::new()))
        }

        JournalCommand::Show { repo, mode, json } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            if let ShowMode::Grep {
                query,
                kinds,
                limit,
            } = mode
            {
                return match run_recorded_search(&repo_root, &query, &kinds, limit, json) {
                    Ok((code, bytes)) => Ok((code, bytes, Vec::new())),
                    Err(error) => Ok((2, Vec::new(), format!("error: {error}\n").into_bytes())),
                };
            }

            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_read(&repo_root, None)?;
            let (_, today_str, current_week) = store.tz.local_date_and_week(now);

            match mode {
                ShowMode::Today => render_show_period(&store, "daily", &today_str, json),
                ShowMode::Week => render_show_period(&store, "weekly", &current_week, json),
                ShowMode::Period(p) => {
                    let p = p.trim();
                    let kind = if regex::Regex::new(r"^\d{4}-W\d{2}$")
                        .expect("pattern")
                        .is_match(p.trim())
                    {
                        "weekly"
                    } else {
                        "daily"
                    };
                    render_show_period(&store, kind, &p, json)
                }
                ShowMode::Blockers => {
                    let rows = store.open_annotations()?;
                    if json {
                        let convert = |r: &crate::store::AnnotationRow| serde_json::json!({"text":r.text,"event_id":r.event_id,"occurred_at":r.occurred_at,"local_date":r.local_date});
                        let value = serde_json::json!({"since":rows.since,"blockers":rows.blockers.iter().map(convert).collect::<Vec<_>>(),"next_actions":rows.next_actions.iter().map(convert).collect::<Vec<_>>()});
                        return Ok((0, json_output(&value)?, Vec::new()));
                    }
                    let mut output = String::new();
                    for (title, items) in [
                        ("Open blockers", rows.blockers),
                        ("Next priorities", rows.next_actions),
                    ] {
                        output.push_str(&format!("{title} (since {}):\n", rows.since));
                        if items.is_empty() {
                            output.push_str("- none recorded\n");
                        }
                        for row in items {
                            output.push_str(&format!(
                                "- {} ({}) [{}]\n",
                                row.text, row.local_date, row.event_id
                            ));
                        }
                    }
                    Ok((0, output.into_bytes(), Vec::new()))
                }
                ShowMode::Decisions { since } => {
                    let report = match store.decisions(since.as_deref(), 200) {
                        Ok(report) => report,
                        Err(error) if error.starts_with("invalid --since") => {
                            return Ok((
                                2,
                                Vec::new(),
                                b"error: --since requires a date as YYYY-MM-DD\n".to_vec(),
                            ));
                        }
                        Err(error) => return Err(error),
                    };
                    if json {
                        let rows:Vec<_>=report.items.iter().map(|r|serde_json::json!({"text":r.decision,"event_id":r.event_id,"event_type":r.event_type,"occurred_at":r.occurred_at,"local_date":r.local_date,"task_id":r.task_id})).collect();
                        return Ok((
                            0,
                            json_output(
                                &serde_json::json!({"since":since,"decisions":rows,"omitted":report.omitted}),
                            )?,
                            Vec::new(),
                        ));
                    }
                    let mut output = format!(
                        "Decisions{}:\n",
                        since.map(|s| format!(" (since {s})")).unwrap_or_default()
                    );
                    if report.items.is_empty() {
                        output.push_str("- none recorded\n");
                    }
                    for row in report.items {
                        let task = if row.task_id.is_empty() {
                            String::new()
                        } else {
                            format!(" task {}", row.task_id)
                        };
                        output.push_str(&format!(
                            "- {} ({}{task}) [{}]\n",
                            row.decision, row.local_date, row.event_id
                        ));
                    }
                    if report.omitted > 0 {
                        output.push_str(&format!(
                            "omitted: {} older decision(s); narrow with --since YYYY-MM-DD\n",
                            report.omitted
                        ));
                    }
                    Ok((0, output.into_bytes(), Vec::new()))
                }
                ShowMode::Recent(n) => {
                    let events = store.recent_events(n.clamp(1, 256) as usize)?;
                    if json {
                        let obj = serde_json::json!({ "events": events });
                        let bytes = json_output(&obj)?;
                        return Ok((0, bytes, Vec::new()));
                    }
                    if events.is_empty() {
                        return Ok((
                            0,
                            b"work-journal: no events recorded\n".to_vec(),
                            Vec::new(),
                        ));
                    }
                    let mut out = String::new();
                    for ev in &events {
                        let eid = ev.get("event_id").and_then(Value::as_str).unwrap_or("");
                        let occ = ev.get("occurred_at").and_then(Value::as_str).unwrap_or("");
                        let et = ev.get("event_type").and_then(Value::as_str).unwrap_or("");
                        let summary = ev["outcome"]["summary"].as_str().unwrap_or("");
                        let verification = ev["verification_status"].as_str().unwrap_or("");
                        out.push_str(&format!(
                            "- {occ} {et}: {summary} ({verification}) [{eid}]\n"
                        ));
                    }
                    Ok((0, out.into_bytes(), Vec::new()))
                }
                ShowMode::Grep { .. } => unreachable!(),
            }
        }

        JournalCommand::Resolve {
            repo,
            event_id,
            how,
        } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            let res = match store.resolve(&event_id, &how, now) {
                Ok(value) => value,
                Err(error)
                    if error.starts_with("no active journal")
                        || error == "resolve requires --how TEXT" =>
                {
                    return Ok((2, Vec::new(), format!("error: {error}\n").into_bytes()));
                }
                Err(error) => return Err(error),
            };
            let mut out = format!(
                "journal: {} blocker on {} as resolved [{}]\n  {}\n",
                if res.created {
                    "recorded the"
                } else {
                    "already recorded the"
                },
                res.superseded_event_id,
                res.correction_event_id,
                res.blocker
            );
            if !res.remaining_matching_blocker_ids.is_empty() {
                out.push_str(&format!(
                    "note: {} other event(s) carry the same blocker text and keep it open: {}\n",
                    res.remaining_matching_blocker_ids.len(),
                    res.remaining_matching_blocker_ids
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok((0, out.into_bytes(), Vec::new()))
        }

        JournalCommand::Sync {
            repo,
            force,
            today,
            recent_days,
            budget,
            max_per_tick,
        } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            {
                let _state_guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
                let store = JournalStore::open_for_write(&repo_root, None)?;
                let _ = store.materialize()?;
            }

            let sync_target = sync_lock_target(&repo_root);
            let _sync_guard = match try_acquire_lock(&sync_target)? {
                TryLockResult::Acquired(g) => g,
                TryLockResult::Busy => {
                    return Ok((0, Vec::new(), Vec::new()));
                }
            };

            let store = JournalStore::open_for_read(&repo_root, None)?;
            let sr = sync_notion(
                &store,
                &SyncOptions {
                    force,
                    include_today: today,
                    recent_days,
                    budget_seconds: budget,
                    max_per_tick,
                },
                now,
                interrupted,
            )?;
            Ok((
                i32::from(sr.failed > 0),
                sr.to_summary_line().into_bytes(),
                Vec::new(),
            ))
        }

        JournalCommand::Distill { repo, dry_run } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = if dry_run {
                JournalStore::open_for_read(&repo_root, None)?
            } else {
                JournalStore::open_for_write(&repo_root, None)?
            };
            let report = store.distill(dry_run, memory_append, now)?;
            let mut out = String::new();
            if dry_run {
                for lesson in &report.lessons {
                    out.push_str(lesson);
                    out.push('\n');
                }
                out.push_str(&format!(
                    "journal distill: {} lesson(s) would be promoted\n",
                    report.promoted
                ));
            } else if report.promoted > 0 {
                out.push_str(&format!(
                    "journal distill: {} lesson(s) promoted, marker advanced\n",
                    report.promoted
                ));
            } else {
                out.push_str("journal distill: nothing to promote\n");
            }
            if report.deduped > 0 {
                out.push_str(&format!(
                    "journal distill: {} candidate(s) already in shared memory and skipped\n",
                    report.deduped
                ));
            }
            if !report.skipped_refused.is_empty() {
                out.push_str(&format!(
                    "journal distill: {} lesson(s) refused by the memory writer and skipped\n",
                    report.skipped_refused.len()
                ));
            }
            if report.dropped > 0 {
                out.push_str(&format!(
                    "journal distill: {} candidate(s) beyond the per-run cap were not promoted\n",
                    report.dropped
                ));
            }
            Ok((0, out.into_bytes(), Vec::new()))
        }
    }
}

fn render_show_period(
    store: &JournalStore,
    kind: &str,
    period: &str,
    json: bool,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let summary = store.summary_text(kind, period)?;
    if json {
        return Ok((
            0,
            json_output(&serde_json::json!({"kind":kind,"period":period,"content":summary}))?,
            Vec::new(),
        ));
    }
    match summary {
        Some(text) => Ok((0, format!("{text}\n").into_bytes(), Vec::new())),
        None => Ok((
            0,
            format!("work-journal: no {kind} summary for {period}\n").into_bytes(),
            Vec::new(),
        )),
    }
}

fn json_output(value: &Value) -> Result<Vec<u8>, String> {
    crate::sanitize::default_json_bytes(&crate::sanitize::to_oms_json(value)?)
}
