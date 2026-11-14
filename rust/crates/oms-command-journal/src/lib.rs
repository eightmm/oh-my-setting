#![forbid(unsafe_code)]

pub mod adapters;
pub mod cli;
pub mod cold;
pub mod config;
pub mod notion;
pub mod render;
pub mod sanitize;
pub mod schema;
pub mod search;
pub mod store;
pub mod tz;

use crate::adapters::build_source_payload;
use crate::cli::{JournalCommand, ShowMode, help_text, parse_command};
use crate::cold::seal_report_text;
use crate::config::{
    configure_notion, expand_user_path, journal_language, notion_settings, purge_notion_config,
    set_journal_language, update_notion_exclusions, work_journal_config_path,
};
use crate::notion::{NotionExporter, SyncOptions, sync_notion};
use crate::sanitize::{os_str_to_py_string, pretty_json_bytes, pretty_json_with_surrogates};
use crate::schema::{
    atomic_write_bytes, canonical_repo_root, detected_project_id, project_id_path, work_journal_dir,
};
use crate::search::run_recorded_search;
use crate::store::{JournalStore, MemoryAppendFn};
pub use crate::store::{MemoryAppendFn as JournalMemoryAppendFn, MemoryAppendOutcome};
use crate::tz::{current_utc_now, parse_date_ymd, parse_iso_week_start_date};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
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
        return (0, help_text().as_bytes().to_vec(), Vec::new());
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
                format!("error: {err}\n")
            };
            (1, Vec::new(), line.into_bytes())
        }
    }
}

fn is_harness_child() -> bool {
    std::env::var("OMS_HARNESS_CHILD").is_ok_and(|v| v == "1")
}

fn non_empty_trimmed_path(raw: Option<&Path>) -> Option<PathBuf> {
    let p = raw?;
    if p.as_os_str().is_empty() {
        return None;
    }
    if let Some(s) = p.to_str() {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        return Some(PathBuf::from(t));
    }
    Some(p.to_path_buf())
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
            let mut lines = Vec::new();
            let mut did_anything = false;

            if let Some(ref lang_val) = lang {
                let path = set_journal_language(lang_val)?;
                lines.push(format!(
                    "journal language: {lang_val} (saved to {})",
                    path.display()
                ));
                lines
                    .push("run `oms journal rebuild` to regenerate existing summaries".to_string());
                did_anything = true;
            }

            if !exclude_repos.is_empty() || !include_repos.is_empty() {
                let ex_paths: Vec<PathBuf> = exclude_repos
                    .iter()
                    .map(|p| resolve_repo(Some(p), cwd))
                    .collect();
                let in_paths: Vec<PathBuf> = include_repos
                    .iter()
                    .map(|p| resolve_repo(Some(p), cwd))
                    .collect();
                let (path, list) = update_notion_exclusions(&ex_paths, &in_paths)?;
                lines.push(format!(
                    "notion exclusions updated ({} excluded repo(s)) in {}",
                    list.len(),
                    path.display()
                ));
                did_anything = true;
            }

            if discover || data_source_id.is_some() {
                let settings = notion_settings();
                let target_ds = if discover {
                    let exporter = NotionExporter::from_settings(settings.clone(), 30.0)?;
                    exporter.discover_work_journal_data_source(interrupted)?
                } else {
                    data_source_id.clone().unwrap_or_default()
                };
                if validate {
                    let mut check_settings = settings.clone();
                    check_settings.data_source_id = target_ds.clone();
                    check_settings.database_id.clear();
                    let exporter = NotionExporter::from_settings(check_settings, 30.0)?;
                    let _ = exporter.validate_target(interrupted)?;
                }
                let path = configure_notion(&target_ds, None)?;
                lines.push(format!(
                    "configured Notion data source: {target_ds} (saved to {})",
                    path.display()
                ));
                did_anything = true;
            }

            if !did_anything {
                let cfg_path = work_journal_config_path();
                let settings = notion_settings();
                let cur_lang = journal_language(None);
                lines.push(format!("config: {}", cfg_path.display()));
                lines.push(format!("language: {cur_lang}"));
                lines.push(format!(
                    "notion configured: {} (transport={})",
                    settings.is_configured(),
                    settings.transport
                ));
            }
            Ok((
                0,
                format!("{}\n", lines.join("\n")).into_bytes(),
                Vec::new(),
            ))
        }

        JournalCommand::Disconnect { managed: _ } => {
            let removed = purge_notion_config()?;
            let msg = if removed {
                "disconnected Notion configuration\n".to_string()
            } else {
                "Notion was not configured\n".to_string()
            };
            Ok((0, msg.into_bytes(), Vec::new()))
        }

        JournalCommand::Identity {
            repo,
            json,
            adopt_detected,
        } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let mut store = JournalStore::open_for_read(&repo_root, None)?;
            if adopt_detected {
                let detected_id = detected_project_id(
                    &store.project.github_repository,
                    &store.project.origin_remote,
                    &repo_root,
                );
                store.project.id = detected_id.clone();
                atomic_write_bytes(
                    &project_id_path(&repo_root),
                    format!("{detected_id}\n").as_bytes(),
                )?;
            }
            if json {
                let mut obj = BTreeMap::new();
                obj.insert(
                    oms_json::PyString::from("project_id"),
                    oms_json::Value::String(oms_json::PyString::from(store.project.id.as_str())),
                );
                obj.insert(
                    oms_json::PyString::from("project_name"),
                    oms_json::Value::String(oms_json::PyString::from(store.project.name.as_str())),
                );
                obj.insert(
                    oms_json::PyString::from("github_repository"),
                    oms_json::Value::String(oms_json::PyString::from(
                        store.project.github_repository.as_str(),
                    )),
                );
                obj.insert(
                    oms_json::PyString::from("origin_remote"),
                    oms_json::Value::String(oms_json::PyString::from(
                        store.project.origin_remote.as_str(),
                    )),
                );
                obj.insert(
                    oms_json::PyString::from("repo_root"),
                    oms_json::Value::String(os_str_to_py_string(repo_root.as_os_str())),
                );
                let mut bytes = pretty_json_with_surrogates(&oms_json::Value::Object(obj))?;
                bytes.push(b'\n');
                return Ok((0, bytes, Vec::new()));
            }
            let text = format!(
                "project_id: {}\nproject_name: {}\norigin_remote: {}\ngithub_repository: {}\n",
                store.project.id,
                store.project.name,
                if store.project.origin_remote.is_empty() {
                    "(none)"
                } else {
                    &store.project.origin_remote
                },
                if store.project.github_repository.is_empty() {
                    "(none)"
                } else {
                    &store.project.github_repository
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
                let mut bytes = pretty_json_bytes(&status)?;
                bytes.push(b'\n');
                return Ok((0, bytes, Vec::new()));
            }
            let active = status
                .get("active_events")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let lang = status
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("en");
            let tz = status
                .get("timezone")
                .and_then(Value::as_str)
                .unwrap_or("UTC");
            let proj = status
                .pointer("/project/id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let proj_name = status
                .pointer("/project/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let healthy = status
                .pointer("/storage/healthy")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let cold = status
                .pointer("/storage/cold_segments")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let configured = status
                .pointer("/notion/configured")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let excluded = status
                .pointer("/notion/excluded")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut text = format!(
                "project: {proj_name} ({proj})\ntimezone: {tz}\nlanguage: {lang}\nactive_events: {active}\nhealthy: {healthy}\ncold_segments: {cold}\nnotion: configured={configured} excluded={excluded}\n"
            );
            if let Some(note) = status
                .pointer("/storage/threshold_note")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                text.push_str(&format!("note: {note}\n"));
            }
            Ok((0, text.into_bytes(), Vec::new()))
        }

        JournalCommand::Observe(mut obs) => {
            if std::env::var("OMS_WORK_JOURNAL").is_ok_and(|v| v == "0")
                || std::env::var("OMS_WORK_JOURNAL_ACTIVE").is_ok_and(|v| v == "1")
            {
                return Ok((0, Vec::new(), Vec::new()));
            }
            let repo_root = resolve_repo(obs.repo.as_deref(), cwd);
            obs.inner.source_file = non_empty_trimmed_path(obs.inner.source_file.as_deref())
                .map(|p| resolve_cli_path(&p, cwd));
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let payload = build_source_payload(&repo_root, &obs.inner, now)?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            let (event, created) = store.record_event(&payload, now)?;
            let eid = event.get("event_id").and_then(Value::as_str).unwrap_or("");
            let msg = if created {
                format!("recorded: {eid}\n")
            } else {
                format!("deduplicated: {eid}\n")
            };
            Ok((0, msg.into_bytes(), Vec::new()))
        }

        JournalCommand::Tick {
            repo,
            local_only,
            digest,
            autodistill,
        } => {
            if std::env::var("OMS_WORK_JOURNAL").is_ok_and(|v| v == "0")
                || std::env::var("OMS_WORK_JOURNAL_ACTIVE").is_ok_and(|v| v == "1")
            {
                return Ok((0, Vec::new(), Vec::new()));
            }
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let mut out = String::new();
            {
                let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
                let store = JournalStore::open_for_write(&repo_root, None)?;
                let _ = store.capture_head_commit(now)?;
                let report = store.materialize()?;
                out.push_str(&format!(
                    "tick: events={} updated={}\n",
                    report.event_count, report.written_periods
                ));
                let mut has_digest = false;
                if digest {
                    if let Some(d) = store.prompt_digest(now)? {
                        has_digest = true;
                        out.push_str(&d);
                        out.push('\n');
                    }
                    store.write_prompt_stamp("digest", has_digest, now)?;
                }
                if autodistill && store.distill_due_today(now)? {
                    let dr = store.distill(false, memory_append, now)?;
                    out.push_str(&format!("distilled: {} lesson(s)\n", dr.promoted));
                }
                store.write_prompt_stamp("tick", has_digest, now)?;
            }

            if !local_only {
                let sync_target = sync_lock_target(&repo_root);
                if let TryLockResult::Acquired(_sync_guard) = try_acquire_lock(&sync_target)? {
                    let store = JournalStore::open_for_read(&repo_root, None)?;
                    let sr = sync_notion(
                        &store,
                        &SyncOptions {
                            force: false,
                            include_today: false,
                            recent_days: Some(crate::sanitize::DIGEST_RECENT_DAYS),
                            budget_seconds: 8.0,
                            max_per_tick: Some(1),
                        },
                        now,
                        interrupted,
                    )?;
                    if sr.enabled && (sr.exported > 0 || sr.failed > 0 || sr.deferred > 0) {
                        out.push_str(&sr.to_summary_line());
                    }
                }
            }
            Ok((0, out.into_bytes(), Vec::new()))
        }

        JournalCommand::Materialize { repo } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            let report = store.materialize()?;
            let msg = format!(
                "materialized: events={} written={} removed={}\n",
                report.event_count, report.written_periods, report.removed_periods
            );
            Ok((0, msg.into_bytes(), Vec::new()))
        }

        JournalCommand::Rebuild { repo } => {
            let repo_root = resolve_repo(repo.as_deref(), cwd);
            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_write(&repo_root, None)?;
            let report = store.rebuild()?;
            let msg = format!(
                "rebuilt: events={} daily={} weekly={}\n",
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
                let (code, bytes) = run_recorded_search(&repo_root, &query, &kinds, limit, json)?;
                return Ok((code, bytes, Vec::new()));
            }

            let _guard = acquire_blocking_lock(&state_lock_target(&repo_root))?;
            let store = JournalStore::open_for_read(&repo_root, None)?;
            let (_, today_str, current_week) = store.tz.local_date_and_week(now);

            match mode {
                ShowMode::Today => render_show_period(&store, "daily", &today_str, json),
                ShowMode::Week => render_show_period(&store, "weekly", &current_week, json),
                ShowMode::Period(p) => {
                    let kind = if parse_date_ymd(&p).is_some() {
                        "daily"
                    } else if parse_iso_week_start_date(&p).is_some() {
                        "weekly"
                    } else {
                        return Err(format!(
                            "invalid --period '{p}'; expected YYYY-MM-DD or YYYY-Www"
                        ));
                    };
                    render_show_period(&store, kind, &p, json)
                }
                ShowMode::Blockers => {
                    let ann = store.open_annotations()?;
                    if json {
                        let mut m = Map::new();
                        m.insert(
                            "blockers".into(),
                            Value::Array(
                                ann.blockers
                                    .iter()
                                    .map(|r| {
                                        serde_json::json!({
                                            "event_id": r.event_id,
                                            "local_date": r.local_date,
                                            "text": r.text,
                                        })
                                    })
                                    .collect(),
                            ),
                        );
                        m.insert(
                            "next_actions".into(),
                            Value::Array(
                                ann.next_actions
                                    .iter()
                                    .map(|r| {
                                        serde_json::json!({
                                            "event_id": r.event_id,
                                            "local_date": r.local_date,
                                            "text": r.text,
                                        })
                                    })
                                    .collect(),
                            ),
                        );
                        let mut bytes = pretty_json_bytes(&Value::Object(m))?;
                        bytes.push(b'\n');
                        return Ok((0, bytes, Vec::new()));
                    }
                    let mut out = String::new();
                    if ann.blockers.is_empty() && ann.next_actions.is_empty() {
                        out.push_str("no open blockers or next actions\n");
                    } else {
                        if !ann.blockers.is_empty() {
                            out.push_str("## Blockers\n");
                            for b in &ann.blockers {
                                out.push_str(&format!(
                                    "- [{}] {} ({})\n",
                                    b.event_id, b.text, b.local_date
                                ));
                            }
                        }
                        if !ann.next_actions.is_empty() {
                            out.push_str("## Next actions\n");
                            for n in &ann.next_actions {
                                out.push_str(&format!(
                                    "- [{}] {} ({})\n",
                                    n.event_id, n.text, n.local_date
                                ));
                            }
                        }
                    }
                    Ok((0, out.into_bytes(), Vec::new()))
                }
                ShowMode::Decisions { since } => {
                    let res = store.decisions(since.as_deref(), 200)?;
                    if json {
                        let rows: Vec<Value> = res
                            .items
                            .iter()
                            .map(|d| {
                                serde_json::json!({
                                    "event_id": d.event_id,
                                    "occurred_at": d.occurred_at,
                                    "local_date": d.local_date,
                                    "decision": d.decision,
                                    "source_type": d.source_type,
                                    "source_id": d.source_id,
                                })
                            })
                            .collect();
                        let obj = serde_json::json!({
                            "decisions": rows,
                            "omitted": res.omitted,
                        });
                        let mut bytes = pretty_json_bytes(&obj)?;
                        bytes.push(b'\n');
                        return Ok((0, bytes, Vec::new()));
                    }
                    if res.items.is_empty() {
                        return Ok((0, b"no recorded decisions\n".to_vec(), Vec::new()));
                    }
                    let mut out = String::new();
                    for d in &res.items {
                        out.push_str(&format!(
                            "- {} [{}] {} ({}:{})\n",
                            d.local_date, d.event_id, d.decision, d.source_type, d.source_id
                        ));
                    }
                    if res.omitted > 0 {
                        out.push_str(&format!(
                            "... and {} earlier decision(s) omitted\n",
                            res.omitted
                        ));
                    }
                    Ok((0, out.into_bytes(), Vec::new()))
                }
                ShowMode::Recent(n) => {
                    let events = store.recent_events(n)?;
                    if json {
                        let obj = serde_json::json!({ "events": events });
                        let mut bytes = pretty_json_bytes(&obj)?;
                        bytes.push(b'\n');
                        return Ok((0, bytes, Vec::new()));
                    }
                    if events.is_empty() {
                        return Ok((0, b"no journal events\n".to_vec(), Vec::new()));
                    }
                    let mut out = String::new();
                    for ev in &events {
                        let eid = ev.get("event_id").and_then(Value::as_str).unwrap_or("");
                        let occ = ev.get("occurred_at").and_then(Value::as_str).unwrap_or("");
                        let et = ev.get("event_type").and_then(Value::as_str).unwrap_or("");
                        let summary = crate::render::event_work_label(ev);
                        out.push_str(&format!("- [{occ}] ({et}) {eid}: {summary}\n"));
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
            let res = store.resolve(&event_id, &how, now)?;
            let mut out = format!(
                "resolved {} via {}\n",
                res.superseded_event_id, res.correction_event_id
            );
            if !res.remaining_matching_blocker_ids.is_empty() {
                out.push_str(&format!(
                    "note: {} other active event(s) still carry the same blocker text ({})\n",
                    res.remaining_matching_blocker_ids.len(),
                    res.remaining_matching_blocker_ids.join(", ")
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
                let _ = store.capture_head_commit(now)?;
                let _ = store.materialize()?;
            }

            let sync_target = sync_lock_target(&repo_root);
            let _sync_guard = match try_acquire_lock(&sync_target)? {
                TryLockResult::Acquired(g) => g,
                TryLockResult::Busy => {
                    return Ok((
                        0,
                        b"sync skipped: another sync holds the lock\n".to_vec(),
                        Vec::new(),
                    ));
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
            Ok((0, sr.to_summary_line().into_bytes(), Vec::new()))
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
            for _lesson in &report.skipped_refused {
                out.push_str("skipped lesson: refused by the memory writer\n");
            }
            if dry_run {
                if report.promoted == 0 {
                    out.push_str("distill dry-run: nothing to promote\n");
                } else {
                    out.push_str(&format!(
                        "distill dry-run: {} candidate lesson(s)\n",
                        report.promoted
                    ));
                    for l in &report.lessons {
                        out.push_str(&format!("- {l}\n"));
                    }
                }
            } else if report.promoted == 0 {
                out.push_str("distill: nothing to promote\n");
            } else {
                out.push_str(&format!(
                    "distilled: {} lesson(s) promoted to shared memory\n",
                    report.promoted
                ));
            }
            if report.dropped > 0 {
                out.push_str(&format!(
                    "note: {} older distill candidate(s) exceeded per-run cap and were skipped\n",
                    report.dropped
                ));
            }
            if report.deduped > 0 {
                out.push_str(&format!(
                    "note: {} candidate(s) already present in shared memory\n",
                    report.deduped
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
        let events = store.load_events_for_period(kind, period)?;
        let obj = serde_json::json!({
            "kind": kind,
            "period": period,
            "summary": summary,
            "events": events,
        });
        let mut bytes = pretty_json_bytes(&obj)?;
        bytes.push(b'\n');
        return Ok((0, bytes, Vec::new()));
    }
    match summary {
        Some(text) => Ok((0, text.into_bytes(), Vec::new())),
        None => Ok((
            0,
            format!("no journal summary for {kind} {period}\n").into_bytes(),
            Vec::new(),
        )),
    }
}
