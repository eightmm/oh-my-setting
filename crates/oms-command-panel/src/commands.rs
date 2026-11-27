use crate::{
    Error, Output, Result, Services, cli::Options, json, parent_only, require, rows, text,
};
use oms_panel_core::{
    NativeCommand, Store, Value, dispatch, metrics, release, results, review, routing, salvage,
    state, terminals,
};
use serde_json::json;
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
pub(crate) fn artifact_rows(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .or_else(|| value["rows"].as_array())
        .map(Vec::as_slice)
        .ok_or_else(|| Error::new("artifact index did not return a row list"))
}
pub(crate) fn query(
    service: &dyn Services,
    command: NativeCommand,
    repo: &Path,
    args: &[&str],
    stop: &AtomicBool,
) -> Result<Value> {
    service.command(command, repo, &strings(args), stop)?.json()
}
fn output(value: Value) -> Result<Output> {
    Ok((0, json(&value)?, vec![]))
}
fn plain(value: String) -> Result<Output> {
    Ok((0, value.into_bytes(), vec![]))
}
pub(crate) fn owner(options: &Options) -> String {
    options
        .get("owner")
        .map(str::to_owned)
        .or_else(|| std::env::var("OMS_PANEL_OWNER").ok())
        .or_else(|| std::env::var("OMS_AGENT").ok())
        .unwrap_or_else(|| "codex".into())
}
pub(crate) fn room(options: &Options) -> Option<String> {
    options
        .get("room")
        .map(str::to_owned)
        .or_else(|| std::env::var("OMS_ROOM_ID").ok().filter(|v| !v.is_empty()))
}
fn main_attempt(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<String> {
    if let Ok(attempt) = std::env::var("OMS_PANEL_MAIN_ATTEMPT") {
        crate::identifier(&attempt)?;
        return Ok(attempt);
    }
    let all = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let owner = owner(options);
    let selected_room = room(options);
    let main = options
        .get("main")
        .map(str::to_owned)
        .or_else(|| std::env::var("OMS_ROOM_PARTICIPANT").ok());
    let matches: Vec<_> = rows(&all)
        .iter()
        .filter(|v| v["tool"] == "panel-main" && v["provider"] == owner && v["terminal"] == false)
        .filter(|v| {
            selected_room
                .as_ref()
                .is_none_or(|room| v["refs"]["panel_room_id"] == *room)
        })
        .filter(|v| {
            main.as_ref()
                .is_none_or(|main| state::main_participant(v) == main)
        })
        .collect();
    require(
        matches.len() == 1,
        "owning main identity is absent or ambiguous",
    )?;
    Ok(text(matches[0], "attempt_id").into())
}
fn policy(source: &Path) -> Result<routing::Policy> {
    let path = source.join("config/panel-routing.json");
    if path.exists() {
        routing::Policy::load(&path)
    } else {
        routing::Policy::bundled()
    }
}
/// Per-watch-session reuse of derived reads; every entry is keyed by a file
/// stamp or digest and nothing is persisted.
pub(crate) struct Session {
    cache: oms_panel_core::cache::Cache,
    attempts: state::AttemptCache,
    metrics: std::sync::Mutex<Option<(std::time::Instant, String, ProviderStatus)>>,
    main_model: std::sync::Mutex<Option<MainModel>>,
}
type ProviderStatus = (Value, metrics::CodexQuotaSources);

struct CodexQuotaReceipt {
    repo: PathBuf,
    room: String,
    main: Option<String>,
    generated_at: String,
    status: Value,
    sources: metrics::CodexQuotaSources,
}

thread_local! {
    // One accepted snapshot is passed by Watch directly to pane observation.
    // The comparison guards consistency; generated_at is not a unique ticket.
    static CODEX_QUOTA: RefCell<Option<CodexQuotaReceipt>> = const { RefCell::new(None) };
}

pub(crate) fn clear_codex_quota_receipt() {
    CODEX_QUOTA.with_borrow_mut(|slot| *slot = None);
}

pub(crate) fn publish_codex_quota_receipt(
    repo: &Path,
    main: Option<&str>,
    snapshot: &state::Snapshot,
    sources: metrics::CodexQuotaSources,
) {
    let report = &snapshot.report;
    let receipt = (|| {
        if snapshot.status != 0 || report["collection"]["ok"] != true {
            return None;
        }
        Some(CodexQuotaReceipt {
            repo: repo.to_path_buf(),
            room: report["room"]["id"].as_str()?.to_owned(),
            main: main.map(str::to_owned),
            generated_at: report["generated_at"].as_str()?.to_owned(),
            status: report["provider_status"].get("codex")?.clone(),
            sources,
        })
    })();
    CODEX_QUOTA.with_borrow_mut(|slot| *slot = receipt);
}

pub(crate) fn take_codex_quota_receipt(
    repo: &Path,
    room: &str,
    main: &str,
    report: &Value,
) -> Option<metrics::CodexQuotaSources> {
    let receipt = CODEX_QUOTA.with_borrow_mut(Option::take)?;
    (receipt.repo == repo
        && receipt.room == room
        && report["room"]["id"] == room
        && receipt
            .main
            .as_deref()
            .is_none_or(|expected| expected == main)
        && report["collection"]["ok"] == true
        && report["collection"]["pending"] != true
        && report["generated_at"] == receipt.generated_at
        && report["provider_status"]["codex"] == receipt.status)
        .then_some(receipt.sources)
}

/// Last verified model for one repository, Room Main and native rollout.
struct MainModel {
    repo: PathBuf,
    room: String,
    attempt: String,
    participant: String,
    consumer: String,
    rollout: PathBuf,
    ident: (u64, u64),
    stamp: String,
    model: Option<String>,
    retained: bool,
    header: Option<String>,
    read_size: u64,
    tokens: Option<u64>,
    tokens_retained: bool,
    tokens_observed_at: Option<f64>,
}

struct MainObservation {
    model: Option<Value>,
    tokens: Option<Value>,
    tokens_observed_at: Option<f64>,
}

fn rollout_ident(meta: &std::fs::Metadata) -> Option<((u64, u64), u64)> {
    use std::os::unix::fs::MetadataExt;
    (meta.is_file() && meta.nlink() == 1).then(|| ((meta.dev(), meta.ino()), meta.size()))
}

fn rollout_header(path: &Path, identity: ((u64, u64), u64), consumer: &str) -> Option<String> {
    use rustix::fs::{self, Mode, OFlags};
    use std::io::{BufRead, BufReader, Read};

    const MAX_HEADER: usize = 256 * 1024;
    let fd = fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(fd);
    if rollout_ident(&file.metadata().ok()?) != Some(identity) {
        return None;
    }
    let mut line = Vec::new();
    BufReader::new(file.take(MAX_HEADER as u64))
        .read_until(b'\n', &mut line)
        .ok()?;
    if line.last() != Some(&b'\n') {
        return None;
    }
    let header = oms_panel_core::decode(&line, MAX_HEADER).ok()?;
    let session = header["payload"]["id"].as_str()?;
    (header["type"] == "session_meta"
        && &oms_panel_core::digest(session.as_bytes())[..32] == consumer)
        .then(|| oms_panel_core::digest(&line))
}
const METRICS_TTL: Duration = Duration::from_secs(15);
impl Session {
    pub(crate) fn new() -> Self {
        Self {
            cache: oms_panel_core::cache::Cache::new(256, Duration::from_secs(3600))
                .expect("static cache limits"),
            attempts: Default::default(),
            metrics: Default::default(),
            main_model: Default::default(),
        }
    }
    pub(crate) fn attempt_cache(&self) -> &state::AttemptCache {
        &self.attempts
    }

    fn index(&self, repo: &Path) -> Result<Vec<Value>> {
        let print = oms_panel_core::cache::stamp(&repo.join(".oms/artifacts/index.jsonl"));
        if let Some(Value::Array(rows)) = self.cache.read("index", &print) {
            return Ok(rows);
        }
        let rows = results::index_rows(repo)?;
        let _ = self
            .cache
            .insert("index".into(), print, Value::Array(rows.clone()));
        Ok(rows)
    }
    #[cfg(test)]
    fn observe_main_model_with(
        &self,
        repo: &Path,
        report: &Value,
        locate: impl Fn(&str) -> Option<PathBuf>,
    ) -> Option<Value> {
        self.observe_main_with(repo, report, locate)?.model
    }

    fn observe_main_with(
        &self,
        repo: &Path,
        report: &Value,
        locate: impl Fn(&str) -> Option<PathBuf>,
    ) -> Option<MainObservation> {
        let mut saved = self.main_model.lock().ok()?;
        let target = Self::main_target(report);
        let Some((room, attempt, participant, consumer)) = target else {
            *saved = None;
            return None;
        };
        let current = saved.as_ref().filter(|v| {
            v.repo == repo
                && v.room == room
                && v.attempt == attempt
                && v.participant == participant
                && v.consumer == consumer
                && std::fs::symlink_metadata(&v.rollout)
                    .ok()
                    .and_then(|meta| rollout_ident(&meta))
                    .is_some_and(|(id, _)| id == v.ident)
        });
        if current.is_none() {
            let located = locate(&consumer).and_then(|rollout| {
                let (ident, _) = rollout_ident(&std::fs::symlink_metadata(&rollout).ok()?)?;
                Some((rollout, ident))
            });
            let Some((rollout, ident)) = located else {
                *saved = None;
                return None;
            };
            *saved = Some(MainModel {
                repo: repo.into(),
                room,
                attempt,
                participant,
                consumer: consumer.clone(),
                rollout,
                ident,
                stamp: String::new(),
                model: None,
                retained: false,
                header: None,
                read_size: 0,
                tokens: None,
                tokens_retained: false,
                tokens_observed_at: None,
            });
        }
        let entry = saved.as_mut()?;
        let stamp = oms_panel_core::cache::stamp(&entry.rollout);
        if entry.stamp != stamp {
            let checked = (|| {
                let meta = std::fs::symlink_metadata(&entry.rollout).ok()?;
                let (ident, size) = rollout_ident(&meta)?;
                if ident != entry.ident {
                    return None;
                }
                let header = rollout_header(&entry.rollout, (ident, size), &consumer)?;
                let (model, tokens, tokens_at) =
                    metrics::codex_turn_observations(&entry.rollout, &consumer)?;
                (stamp == oms_panel_core::cache::stamp(&entry.rollout))
                    .then_some((header, size, tokens_at, model, tokens))
            })();
            let Some((header, size, tokens_at, model, tokens)) = checked else {
                *saved = None;
                return None;
            };
            if let Some(model) = model {
                entry.model = Some(model);
                entry.retained = false;
            } else if entry.header.as_ref() == Some(&header) && size > entry.read_size {
                // Same header and named generation with growth. Without a
                // complete turn in the bounded tail, it is only last known.
                entry.retained = entry.model.is_some();
            } else {
                entry.model = None;
                entry.retained = false;
            }
            if let Some(tokens) = tokens {
                entry.tokens = Some(tokens);
                entry.tokens_retained = false;
                entry.tokens_observed_at = tokens_at;
            } else if entry.header.as_ref() == Some(&header) && size > entry.read_size {
                entry.tokens_retained = entry.tokens.is_some();
                entry.tokens_observed_at = None;
            } else {
                entry.tokens = None;
                entry.tokens_retained = false;
                entry.tokens_observed_at = None;
            }
            entry.header = Some(header);
            entry.read_size = size;
            entry.stamp = stamp;
        }
        let source = |retained| if retained { "rollout-last" } else { "rollout" };
        Some(MainObservation {
            model: entry.model.as_ref().map(|model| {
                json!({"attempt":entry.attempt,"model":model,"source":source(entry.retained)})
            }),
            tokens: entry.tokens.map(|tokens| {
                json!({
                    "room":entry.room,"participant":entry.participant,"attempt":entry.attempt,
                    "total":tokens,"source":source(entry.tokens_retained)
                })
            }),
            tokens_observed_at: (!entry.tokens_retained)
                .then_some(entry.tokens_observed_at)
                .flatten(),
        })
    }
    fn main_target(report: &Value) -> Option<(String, String, String, String)> {
        let room = &report["room"];
        if text(room, "id").is_empty() || room["closed"] != false || !room["successor"].is_null() {
            return None;
        }
        let mains = state::active_mains(
            rows(&report["attempts"]["active"]),
            text(room, "id"),
            "codex",
        );
        let [main] = mains.as_slice() else {
            return None;
        };
        if !state::LIVE_STATES.contains(&text(main, "state")) {
            return None;
        }
        let participant = state::main_participant(main);
        let mut joined = rows(&room["participants"])
            .iter()
            .filter(|member| member["role"] == "main" && member["joined"] == true);
        let member = joined.next()?;
        if joined.next().is_some()
            || member["participant"] != participant
            || member["provider"] != "codex"
            || !member["parent"].is_null() && member["parent"] != ""
            || room["owner"]
                .as_str()
                .is_some_and(|owner| !owner.is_empty() && owner != participant)
        {
            return None;
        }
        Some((
            text(room, "id").into(),
            main["attempt_id"].as_str()?.into(),
            participant.into(),
            member["consumer"].as_str()?.into(),
        ))
    }
    fn provider_status(
        &self,
        repo: &Path,
        report: &Value,
        main: Option<&str>,
        rows: &[Value],
    ) -> ProviderStatus {
        let candidates = main_candidates_key(report).to_string();
        let main_id = format!("{main:?}");
        let index = oms_panel_core::cache::stamp(&repo.join(".oms/artifacts/index.jsonl"));
        let sessions = sessions_stamp();
        let registry = codex_registry_stamp();
        let print = oms_panel_core::cache::fingerprint(&[
            ("repo", repo.as_os_str().as_encoded_bytes()),
            ("candidates", candidates.as_bytes()),
            ("main", main_id.as_bytes()),
            ("index", index.as_bytes()),
            ("sessions", sessions.as_bytes()),
            ("registry", registry.as_bytes()),
        ]);
        if let Ok(guard) = self.metrics.lock()
            && let Some((at, key, value)) = guard.as_ref()
            && *key == print
            && at.elapsed() < METRICS_TTL
        {
            return value.clone();
        }
        let value = metrics::collect_local_with_sources(report, main, unix_now(), Some(rows));
        if let Ok(mut guard) = self.metrics.lock() {
            *guard = Some((std::time::Instant::now(), print, value.clone()));
        }
        value
    }
}
/// Only the rows the collector can select as a Codex/Claude main feed the cache key.
fn main_candidates_key(report: &Value) -> Value {
    let keys = [
        "participant",
        "attempt_id",
        "provider",
        "state",
        "consumer",
        "account",
        "model",
    ];
    let rows = rows(&report["room"]["participants"])
        .iter()
        .filter(|v| {
            v["role"] == "main" && matches!(v["provider"].as_str(), Some("codex" | "claude"))
        })
        .map(|v| {
            Value::Object(
                keys.iter()
                    .filter_map(|k| v.get(k).map(|value| ((*k).into(), value.clone())))
                    .collect(),
            )
        })
        .collect();
    json!([report["room"]["id"], Value::Array(rows)])
}
fn sessions_stamp() -> String {
    let home = |name: &str, sub: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(sub)))
    };
    let codex = home("CODEX_HOME", ".codex").map(|p| p.join("sessions"));
    let claude = home("CLAUDE_CONFIG_DIR", ".claude").map(|p| p.join("projects"));
    format!(
        "{}|{}",
        codex.map_or("-".into(), |p| oms_panel_core::cache::stamp(&p)),
        claude.map_or("-".into(), |p| oms_panel_core::cache::stamp(&p))
    )
}

fn codex_registry_stamp() -> String {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map_or_else(
            || "-".into(),
            |path| oms_panel_core::cache::stamp(&path.join("oh-my-setting/provider-accounts.json")),
        )
}

pub(crate) fn snapshot(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<state::Snapshot> {
    clear_codex_quota_receipt();
    snapshot_for(
        &Session::new(),
        repo,
        room(options).as_deref(),
        options.get("owner-attempt"),
        services,
        stop,
        false,
    )
    .map(|(snapshot, _, _)| snapshot)
}
pub(crate) fn snapshot_with(
    session: &Session,
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<state::Snapshot> {
    clear_codex_quota_receipt();
    let main = options.get("owner-attempt");
    let (snapshot, sources, _) = snapshot_for(
        session,
        repo,
        room(options).as_deref(),
        main,
        services,
        stop,
        false,
    )?;
    if options.has("watch") && !options.has("log") {
        publish_codex_quota_receipt(repo, main, &snapshot, sources);
    }
    Ok(snapshot)
}

pub(crate) fn snapshot_for(
    session: &Session,
    repo: &Path,
    room_id: Option<&str>,
    main_attempt: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
    include_write_slots: bool,
) -> Result<(
    state::Snapshot,
    metrics::CodexQuotaSources,
    Option<oms_panel_core::dispatch::WriteSlots>,
)> {
    let (mut snapshot, write_slots) = state::snapshot_with_write_slots(
        repo,
        room_id,
        services,
        stop,
        session.attempt_cache(),
        main_attempt.filter(|_| include_write_slots),
    )?;
    let calls = crate::render::calls(&snapshot.report);
    let tasks: Vec<_> = calls
        .iter()
        .filter_map(|v| {
            v["task_id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .collect();
    let read_reviews: std::collections::BTreeSet<_> = calls
        .iter()
        .filter(|call| call["state"] == "review" && call["refs"]["panel_access"] == "read")
        .map(|call| text(call, "attempt"))
        .filter(|attempt| !attempt.is_empty())
        .collect();
    let store = Store::open(repo)?;
    let index = session.index(repo)?;
    // Bind outcomes to the plan this refresh displays, never a second read.
    let decisions = results::outcomes_cached(
        &store,
        &snapshot.report["plan"],
        &index,
        &tasks,
        Some((&session.cache, repo)),
    )?;
    let mut finalized = serde_json::Map::new();
    for decision in decisions {
        if let Some(task) = decision["task_id"].as_str() {
            finalized.insert(task.into(), decision["outcome"].clone());
        }
    }
    snapshot.report["finalized"] = Value::Object(finalized);
    snapshot.report["returned_workers"] = Value::Object(
        index
            .iter()
            .filter(|row| {
                row["kind"] == "delegate"
                    && row["exit"].as_i64() == Some(0)
                    && row["artifact"].is_string()
                    && row["artifact_sha256"].is_string()
                    && read_reviews.contains(text(row, "attempt_id"))
            })
            .filter_map(|row| {
                Some((
                    row["attempt_id"].as_str()?.into(),
                    row["ts"].as_str()?.into(),
                ))
            })
            .collect(),
    );
    let main_attempt = main_attempt.or_else(|| snapshot.report["room"]["main"].as_str());
    let (provider_status, quota_sources) =
        session.provider_status(repo, &snapshot.report, main_attempt, &index);
    snapshot.report["provider_status"] = provider_status;
    if let Some(observation) =
        session.observe_main_with(repo, &snapshot.report, metrics::codex_rollout)
    {
        if let Some(model) = observation.model {
            snapshot.report["main_model_observation"] = model;
        }
        if let Some(mut tokens) = observation.tokens {
            if let Some(at) = observation.tokens_observed_at {
                tokens["observed_at"] = json!(at);
            }
            snapshot.report["main_token_observation"] = tokens;
        }
    }
    let plan = &snapshot.report["plan"];
    if plan["tasks"].as_object().is_some_and(|tasks| {
        tasks.values().any(|task| {
            matches!(text(task, "state"), "claimed" | "running")
                || (task["state"] == "review"
                    && (task["pr"].is_string() || task["issue"].is_string()))
        })
    }) {
        let projected = match query(
            services,
            NativeCommand::Plan,
            repo,
            &["list", "--json"],
            stop,
        ) {
            Ok(list) => bind_plan_attention(plan, &list),
            Err(err) if err.code == 130 => return Err(err),
            // A missing entry in this map is an unavailable observation.
            Err(_) => json!({}),
        };
        snapshot.report["plan_attention"] = projected;
    }
    Ok((snapshot, quota_sources, write_slots))
}

fn bind_plan_attention(plan: &Value, list: &Value) -> Value {
    let mut projected = serde_json::Map::new();
    if list["schema"] == 1 && text(plan, "plan_id") == text(list, "plan_id") {
        for task in rows(&list["tasks"]) {
            let id = text(task, "id");
            let stored = &plan["tasks"][id];
            if !matches!(text(stored, "state"), "claimed" | "running" | "review")
                || !task["claim_expired"].is_boolean()
            {
                continue;
            }
            // The watch snapshot retains the stored Plan for evidence joins.
            // Apply display fields only to the exact task the Plan read checked.
            if serde_json::to_vec(stored).is_ok_and(|raw| {
                oms_command_plan::stored_task_revision(&raw)
                    .is_ok_and(|rev| rev == text(task, "task_sha256"))
            }) {
                projected.insert(
                    id.to_owned(),
                    json!({
                        "claim_expired": task["claim_expired"],
                        "pr_review_candidate": task["pr_review_candidate"],
                    }),
                );
            }
        }
    }
    Value::Object(projected)
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn parse_float_option(options: &Options, name: &str) -> Result<Option<f64>> {
    let Some(raw) = options.get(name) else {
        return Ok(None);
    };
    let val = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| Error::new(format!("--{name} must be a non-negative number")))?;
    require(
        val.is_finite() && val >= 0.0,
        &format!("--{name} must be a non-negative number"),
    )?;
    Ok(Some(val))
}

pub fn resolve_repositories_with(
    options: &Options,
    cwd: &Path,
    env_room_raw: Option<&Path>,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    resolve_repositories(options, cwd, env_room_raw, false)
}

pub(crate) fn resolve_repositories(
    options: &Options,
    cwd: &Path,
    env_room_raw: Option<&Path>,
    selecting_main: bool,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let explicit_raw = options.get("repo").map(|raw| {
        let p = PathBuf::from(raw);
        if p.is_absolute() { p } else { cwd.join(p) }
    });
    let optional_git = options.command.is_none()
        && ((options.has("providers") || options.has("routes")) && explicit_raw.is_none()
            || (options.has("recommend") && !options.has("diff") && explicit_raw.is_none()));
    let requires_room_match = selecting_main
        || options.command.is_some()
        || options.has("dispatch")
        || options.has("continue")
        || options.has("salvage")
        || options.has("accept")
        || options.has("finalize")
        || options.has("retry-delivery")
        || options.has("council")
        || options.has("settle-review")
        || options.has("launch")
        || options.has("spawn-main")
        || options.has("orchestrator")
        || options.has("native-reconcile")
        || options.has("native-adopt")
        || options.has("focus-orchestrator")
        || options.has("reopen")
        || options.has("swap-main")
        || options.has("release-main")
        || options.has("mail")
        || options.has("chats")
        || options.has("open-chat");

    let env_room = env_room_raw
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| state::repository_for(p, true))
        .transpose()
        .map_err(|e| Error { code: 3, ..e })?;

    if let Some(raw_exec) = explicit_raw {
        let (exec_root, is_git) =
            state::work_repository(&raw_exec).map_err(|e| Error { code: 3, ..e })?;
        if !is_git {
            return Err(Error {
                code: 3,
                ..Error::new(format!(
                    "--repo is not a Git repository: {}",
                    raw_exec.display()
                ))
            });
        }
        let state_repo = state::repository_for(&raw_exec, !requires_room_match)
            .map_err(|e| Error { code: 3, ..e })?;
        if requires_room_match
            && let Some(ref room_repo) = env_room
            && &state_repo != room_repo
        {
            return Err(Error::new(format!(
                "--repo ({}) conflicts with OMS_ROOM_REPO ({})",
                state_repo.display(),
                room_repo.display()
            )));
        }
        return Ok((Some(state_repo), Some(exec_root)));
    }

    if optional_git {
        let cwd_repo = state::repository_for(cwd, true).ok();
        let exec_root = state::work_repository(cwd)
            .ok()
            .filter(|(_, g)| *g)
            .map(|(p, _)| p);
        return Ok((cwd_repo.or(env_room), exec_root));
    }

    let (work_root, is_git) = state::work_repository(cwd).map_err(|e| Error { code: 3, ..e })?;
    let fallback_state = if is_git {
        state::repository_for(cwd, !requires_room_match).map_err(|e| Error { code: 3, ..e })?
    } else if let Some(ref room_repo) = env_room {
        room_repo.clone()
    } else {
        return Err(Error {
            code: 3,
            ..Error::new("not a Git repository (use --repo or set OMS_ROOM_REPO)")
        });
    };
    let exec_root = if is_git {
        work_root
    } else {
        env_room.clone().unwrap_or_else(|| fallback_state.clone())
    };
    Ok((Some(env_room.unwrap_or(fallback_state)), Some(exec_root)))
}
pub fn run(
    options: &Options,
    cwd: &Path,
    source: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    run_mode(options, false, cwd, source, services, stop)
}

pub(crate) fn run_mode(
    options: &Options,
    default_main: bool,
    cwd: &Path,
    source: &Path,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    if options.has("register-project")
        || options.has("unregister-project")
        || options.has("projects")
    {
        return crate::desktop_projects::run(options, cwd, stop);
    }
    if options.has("desktop-view") {
        // The desktop connection selects the checkout independently of process-wide Room variables.
        let (state, execution) = resolve_repositories(options, cwd, None, false)?;
        let (repo, execution) = state
            .zip(execution)
            .ok_or_else(|| Error::new("desktop view needs a selected repository"))?;
        return crate::desktop_board::run(&repo, &execution, source, options, services, stop);
    }
    if options.has("desktop-worker-output") {
        let selected = cwd
            .join(options.required("repo")?)
            .canonicalize()
            .map_err(Error::new)?;
        let (state, _) = resolve_repositories(options, cwd, None, false)?;
        require(
            state.as_ref() == Some(&selected),
            "worker output needs an exact project root",
        )?;
        return output(crate::desktop_detail::worker_output(
            &selected, options, services, stop,
        )?);
    }
    if options.has("desktop-work-items") {
        // The desktop connection selects the checkout independently of process-wide Room variables.
        let (state, _) = resolve_repositories(options, cwd, None, false)?;
        let repo =
            state.ok_or_else(|| Error::new("desktop work items needs a selected repository"))?;
        return crate::desktop_board::work_items(&repo, services, stop);
    }
    let env_room = std::env::var_os("OMS_ROOM_REPO").map(PathBuf::from);
    let (repo_opt, exec_opt) =
        resolve_repositories(options, cwd, env_room.as_deref(), default_main)?;
    if default_main
        && let Some(repo) = &repo_opt
        && let Some(output) =
            crate::native_panel::default_main(repo, cwd, source, options, services, stop)?
    {
        return Ok(output);
    }
    if let Some(ref _cmd) = options.command {
        let repo = repo_opt.ok_or_else(|| Error {
            code: 3,
            ..Error::new("repository required")
        })?;
        return crate::managed::run(options, cwd, &repo, source, services, stop);
    }
    if options.has("recommend") {
        let exec = exec_opt.unwrap_or_else(|| cwd.to_path_buf());
        return crate::recommend::run(&exec, source, options, services, stop);
    }
    if options.managed_plan_task() {
        let repo = repo_opt.ok_or_else(|| Error::new("repository required"))?;
        let execution = exec_opt.ok_or_else(|| Error::new("execution repository required"))?;
        return crate::orchestrator::run_managed_plan(&repo, &execution, options, services, stop);
    }
    if options.has("dry-run")
        && !options.has("dispatch")
        && !options.has("continue")
        && !options.has("council")
        && !options.has("open-chat")
    {
        let repo = repo_opt.unwrap_or_else(|| cwd.to_path_buf());
        return crate::native_panel::dry_run_plan(&repo, source, options, services, stop);
    }
    if options.has("settle-review") {
        let repo = repo_opt.ok_or_else(|| Error {
            code: 3,
            ..Error::new("repository required")
        })?;
        let req = review::SettleReviewRequest {
            owner: options.get("owner").map(str::to_owned),
            owner_attempt: options.get("owner-attempt").map(str::to_owned),
            explicit: options.all("attempt"),
            reason: options.get("reason").map(str::to_owned),
            apply: options.has("apply"),
            legacy: options.has("legacy"),
            legacy_older_than: parse_float_option(options, "legacy-older-than")?,
            standalone: options.has("standalone"),
            older_than: parse_float_option(options, "older-than")?,
            aged_days: parse_float_option(options, "aged-days")?,
        };
        let result = review::settle_review(&repo, &req, services, stop)?;
        return output(result);
    }
    if options.has("launch") || options.has("spawn-main") || options.has("reopen") {
        let repo = repo_opt.ok_or_else(|| Error {
            code: 3,
            ..Error::new("repository required")
        })?;
        return crate::native_panel::run(&repo, source, options, services, stop);
    }
    if options.has("native-reconcile") || options.has("native-adopt") {
        let repo = repo_opt.ok_or_else(|| Error::new("repository required"))?;
        return crate::orchestrator::run(&repo, "codex", options, services, stop);
    }
    if let Some(provider) = options
        .get("orchestrator")
        .or(options.get("focus-orchestrator"))
    {
        let repo = repo_opt.ok_or_else(|| Error::new("repository required"))?;
        return crate::orchestrator::run(&repo, provider, options, services, stop);
    }
    let repo = repo_opt;
    let execution = exec_opt;
    let state_repo = repo.as_deref();
    let _execution_path = execution.as_deref();
    let repo_req = || {
        repo.clone().ok_or_else(|| Error {
            code: 3,
            ..Error::new("repository required")
        })
    };
    let execution_req = || {
        execution
            .clone()
            .or_else(|| repo.clone())
            .ok_or_else(|| Error {
                code: 3,
                ..Error::new("repository required")
            })
    };
    let _ = state_repo;
    if options.has("providers") {
        let query_repo = repo.as_deref().unwrap_or(cwd);
        let report = query(
            services,
            NativeCommand::ProviderCatalog,
            query_repo,
            &["--json"],
            stop,
        )?;
        let providers = report.get("providers").unwrap_or(&report).clone();
        return if options.has("json") {
            output(
                json!({"schema":1,"kind":"oms-panel-providers","providers":providers,"executes":false}),
            )
        } else {
            plain(
                crate::render::value_lines(&providers, crate::layout::dimensions().0).join("\n")
                    + "\n",
            )
        };
    }
    if options.has("routes") {
        return routes_output(
            repo.as_deref(),
            execution.is_some(),
            cwd,
            source,
            options,
            services,
            stop,
        );
    }
    let repo = repo_req()?;
    let execution = execution_req()?;
    if options.has("teams") {
        let report = oms_panel_core::teams::read_at(&execution, &repo, services, stop)?;
        if options.has("json") {
            return output(report);
        }
        if std::env::var_os("OMS_PANEL_READER_BOARD").is_some()
            && let Some(mut tty) = crate::input::TerminalInput::open()?
        {
            crate::reader::page(&repo, &report, &mut tty, stop)?;
            return Ok((0, vec![], vec![]));
        }
        return plain(crate::render::teams(&report, crate::layout::dimensions().0));
    }
    if options.has("release-main") {
        return output(release::release(
            &repo,
            &options.required("release-main")?,
            services,
            stop,
        )?);
    }
    if options.has("swap-main") {
        let harness = options.required("swap-main")?;
        let provider = if harness == "agy" {
            "antigravity"
        } else if harness == "pi" {
            "codex"
        } else {
            &harness
        }
        .to_owned();
        let model = options
            .get("model")
            .map(str::to_owned)
            .or_else(|| {
                policy(source)
                    .ok()
                    .and_then(|p| p.main.get(&provider).map(|v| v.model.clone()))
            })
            .ok_or_else(|| Error::new("swap target model unavailable"))?;
        return output(release::swap_main_in_room(
            &repo,
            &release::MainSwapRequest {
                attempt_id: main_attempt(&repo, options, services, stop)?,
                provider,
                model,
                harness,
            },
            room(options).as_deref(),
            services,
            stop,
        )?);
    }
    if options.has("results") {
        let report = results_report(&repo, options, services, stop)?;
        if options.has("json") {
            return output(report);
        }
        let text = crate::render::results(
            &report,
            if options.has("export") {
                100
            } else {
                crate::layout::dimensions().0
            },
        );
        if let Some(path) = options.get("export") {
            let path = Path::new(path);
            let path = if path.is_absolute() {
                path.into()
            } else {
                cwd.join(path)
            };
            oms_panel_core::export::write(&path, &text)?;
            return Ok((0, vec![], vec![]));
        }
        if std::env::var_os("OMS_PANEL_READER_BOARD").is_some()
            && let Some(mut tty) = crate::input::TerminalInput::open()?
        {
            crate::reader::page(&repo, &report, &mut tty, stop)?;
            return Ok((0, vec![], vec![]));
        }
        return plain(text);
    }
    if options.has("accept") {
        let request = results::AcceptRequest {
            owner: owner(options),
            task_id: options.required("accept")?,
            main_attempt_id: main_attempt(&repo, options, services, stop)?,
            message: options.required("message")?,
            verify: options.required("verify")?,
            execution_repo: execution.clone(),
            expected_patch_sha256: options.get("expected-patch-sha256").map(str::to_owned),
            allow_verifier_change: options.has("allow-verifier-change"),
            allow_floor_change: options.get("allow-floor-change").map(str::to_owned),
        };
        let accepted = results::accept(&repo, &request, services, stop)?;
        return plain(acceptance_output(&request, &accepted));
    }
    if options.has("finalize") {
        let summary = PathBuf::from(options.required("summary-file")?);
        return output(results::finalize(
            &repo,
            &results::FinalizeRequest {
                owner: owner(options),
                task_id: options.required("task-id")?,
                main_attempt_id: main_attempt(&repo, options, services, stop)?,
                summary_file: if summary.is_absolute() {
                    summary
                } else {
                    execution.join(summary)
                },
                outcome: options.required("outcome")?,
                verify: options.get("verify").map(str::to_owned),
                evidence: options
                    .all("evidence")
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
                verify_timeout: Duration::from_secs(options.integer(
                    "verify-timeout",
                    1800,
                    1,
                    86400,
                )? as u64),
                execution_repo: execution,
                notify: options.has("notify"),
                landed_commit: options.get("landed-commit").map(str::to_owned),
            },
            services,
            stop,
        )?);
    }
    if options.has("retry-delivery") {
        return output(results::retry_delivery(
            &repo,
            &options.required("task-id")?,
            services,
            stop,
        )?);
    }
    if options.has("dispatch") || options.has("continue") {
        return dispatch_command(&repo, &execution, source, options, services, stop);
    }
    if options.has("council") {
        let prompt = options.required("prompt")?;
        let task_id = options
            .get("task-id")
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "council-{}",
                    &oms_panel_core::digest(prompt.as_bytes())[..8]
                )
            });
        let label = options.get("label").map(str::to_owned).unwrap_or_else(|| {
            let first = prompt.lines().next().unwrap_or("").trim();
            let mut label: String = first.chars().take(60).collect();
            if first.chars().count() > 60 {
                label.push('…');
            }
            label
        });
        let request = review::CouncilRequest {
            owner: owner(options),
            task_id,
            label,
            prompt,
            thread: options.get("thread").map(str::to_owned),
            rounds: options
                .get("rounds")
                .map(|_| options.integer("rounds", 1, 0, 2))
                .transpose()?,
            workload: options.text("workload", "deep"),
            lenses: options
                .get("lenses")
                .map(|v| v.split(',').map(str::to_owned).collect()),
            until_agreement: options.has("until-agreement"),
            max_rounds: options
                .get("max-rounds")
                .map(|_| options.integer("max-rounds", 4, 1, 6))
                .transpose()?,
            allow_partial: false,
            dry_run: options.has("dry-run"),
            room: room(options),
            main_attempt: if options.has("dry-run") {
                None
            } else {
                Some(main_attempt(&repo, options, services, stop)?)
            },
            participant: std::env::var("OMS_ROOM_PARTICIPANT").ok(),
        };
        return output(review::council_execute(
            &repo,
            &request,
            &policy(source)?,
            services,
            stop,
        )?);
    }
    if options.has("salvage") {
        return salvage_command(&repo, &execution, options, services, stop);
    }
    if options.has("mail") {
        return mail(&repo, options, services, stop);
    }
    if options.has("desktop-chat") {
        return output(crate::desktop_detail::chat(&repo, options, services, stop)?);
    }
    if options.has("chats") || options.has("open-chat") {
        return chats(&repo, options, services, stop);
    }
    if options.has("watch") || options.has("log") {
        return crate::watch::run_with_source(&repo, source, options, services, stop);
    }
    if options.has("desktop-usage") {
        // Desktop project selection never inherits a process-wide Room selection.
        let (state, _, _) =
            snapshot_for(&Session::new(), &repo, None, None, services, stop, false)?;
        return output(crate::desktop_detail::usage(
            &repo,
            &state.report,
            unix_now(),
            stop,
        ));
    }
    let state = snapshot(&repo, options, services, stop)?;
    if options.has("health") {
        let mut health =
            oms_panel_core::health::project(&state.report, oms_panel_core::timestamp() as f64);
        health["work"] = oms_panel_core::health::work_health(&state.report);
        return if options.has("json") {
            output(health)
        } else {
            plain(crate::render::value_lines(&health, 120).join("\n") + "\n")
        };
    }
    if options.has("metrics") {
        let attempts: Vec<_> = rows(&state.report["attempts"]["recent"]).to_vec();
        let artifacts = query(
            services,
            NativeCommand::Telemetry,
            &repo,
            &["list", "--json"],
            stop,
        )?;
        let tasks: Vec<_> = state.report["plan"]["tasks"]
            .as_object()
            .map(|v| v.values().cloned().collect())
            .unwrap_or_else(|| rows(&state.report["plan"]["tasks"]).to_vec());
        let metrics = oms_panel_core::metrics::aggregate(&oms_panel_core::metrics::MetricInputs {
            attempts: &attempts,
            artifacts: artifact_rows(&artifacts)?,
            tasks: &tasks,
            since: options.get("since"),
            now: oms_panel_core::timestamp() as f64,
            min_samples: options.integer("min-samples", 3, 1, usize::MAX)?,
            incomplete_reasons: vec![],
        })?;
        return if options.has("json") {
            output(metrics)
        } else {
            plain(oms_panel_core::metrics::metrics_text(&metrics) + "\n")
        };
    }
    if options.has("sessions") {
        return sessions(&state.report, options);
    }
    if options.has("usage") {
        let snap = crate::usage::explicit_snapshot(
            &state.report,
            unix_now(),
            &crate::orchestrator::account_binding::SystemAccountHost,
        );
        if options.has("json") {
            return Ok((state.status, json(&snap)?, vec![]));
        }
        return Ok((
            state.status,
            format!("{}\n", crate::usage::text(&snap)).into_bytes(),
            vec![],
        ));
    }
    if options.has("compare") {
        return comparison(&repo_req()?, options, services, stop);
    }
    if options.has("recommend") {
        return crate::recommend::run(&execution_req()?, source, options, services, stop);
    }
    if options.has("next-attention") {
        return next_attention(&repo_req()?, options, services, stop);
    }
    if options.has("json") {
        return Ok((state.status, json(&state.report)?, vec![]));
    }
    if options.has("dry-run") {
        return crate::native_panel::dry_run_plan(&repo_req()?, source, options, services, stop);
    }
    crate::watch::run_with_source(&repo_req()?, source, options, services, stop)
}

pub(crate) fn results_report(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let task = options.get("task-id").map(str::to_owned);
    results::results_page(
        repo,
        &results::ResultsRequest {
            task_id: task,
            room_participant: options.get("participant").map(str::to_owned),
            offset: (options.integer("page", 1, 1, 1000)? - 1) * 20,
            ..Default::default()
        },
        services,
        stop,
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn routes_report(
    repo: Option<&Path>,
    has_git_repo: bool,
    cwd: &Path,
    source: &Path,
    options: &Options,
    selected_room: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Value> {
    let policy = policy(source)?;
    let owner = owner(options);
    let (inputs, events_observed) =
        routing_inputs(repo, Some(cwd), selected_room, Some(&owner), services, stop)?;
    // The resolver may use a non-Git directory for the existing preview. That
    // fallback does not establish repository-wide count evidence.
    let events_observed = has_git_repo && events_observed;
    let events_source = if !has_git_repo {
        "no_repository"
    } else if events_observed {
        "repository_events"
    } else {
        "unavailable"
    };
    let mut base = serde_json::to_value(&policy).map_err(Error::new)?;
    let mut kinds: Vec<String> = policy.roles.keys().cloned().collect();
    kinds.sort();
    let mut writers: Vec<String> = routing::WRITERS.iter().map(|s| (*s).to_string()).collect();
    writers.sort();
    let mut chosen = serde_json::Map::new();
    for role in &kinds {
        let access = if routing::WRITERS.contains(&role.as_str()) {
            "write"
        } else {
            "read"
        };
        let mut trace = routing::AllocationEvidence::default();
        let selected =
            routing::allocate_preview(&policy, &owner, role, access, &inputs, &mut trace);
        let mut allocation = serde_json::to_value(&trace).map_err(Error::new)?;
        if let Some(candidates) = allocation["candidates"].as_array_mut() {
            for (candidate, traced) in candidates.iter_mut().zip(&trace.candidates) {
                let provider = &traced.provider;
                candidate["owner_provider"] = json!(provider == &owner);
                candidate["non_main_attempts"] = json!(
                    events_observed.then(|| inputs.recent.get(provider).copied().unwrap_or(0))
                );
                candidate["in_flight"] = json!(
                    events_observed.then(|| inputs.in_flight.get(provider).copied().unwrap_or(0))
                );
            }
        }
        match selected {
            Ok((alloc, rank)) => {
                let provider = &alloc.candidate.provider;
                let launch_account = if routing::ACCOUNT_PROVIDERS.contains(&provider.as_str()) {
                    "headless_may_reselect"
                } else {
                    "no_account_transport"
                };
                let account_rank = rank.map(|rank| json!({
                    "policy": rank.policy,
                    "eligible_accounts": rank.eligible_accounts,
                    "decided_by": rank.decided_by,
                    "weekly_used_bucket": rank.weekly_bucket,
                    "next_weekly_used_bucket": rank.next_weekly_bucket,
                    "repo_working_attempts": rank.working_attempts.filter(|_| events_observed),
                    "next_repo_working_attempts": rank.next_working_attempts.filter(|_| events_observed),
                }));
                chosen.insert(
                    role.clone(),
                    json!({
                        "provider": alloc.candidate.provider,
                        "model": alloc.candidate.model,
                        "effort": alloc.candidate.effort,
                        "snapshot": alloc.snapshot,
                        "evidence": {
                            "provider_disposition": "provisional",
                            "role": alloc.role,
                            "tier": alloc.workload,
                            "events_source": events_source,
                            "recent": {
                                "scope": if selected_room.is_some() { "room" } else { "repository" },
                                "non_main_attempts": events_observed.then(|| inputs.recent.get(provider).copied().unwrap_or(0)),
                                "window_seconds": ROUTE_RECENT_SECONDS,
                                "in_flight": events_observed.then(|| inputs.in_flight.get(provider).copied().unwrap_or(0)),
                            },
                            "account_reason": alloc.account_reason,
                            "launch_account": launch_account,
                            "account_rank": account_rank,
                            "allocation": allocation,
                        },
                    }),
                );
            }
            Err(err) => {
                chosen.insert(
                    role.clone(),
                    json!({
                        "error": err.message,
                        "evidence": {"events_source": events_source, "allocation": allocation},
                    }),
                );
            }
        }
    }
    if let Some(obj) = base.as_object_mut() {
        obj.insert("writers".into(), json!(writers));
        obj.insert("kinds".into(), json!(kinds));
        obj.insert("owner".into(), json!(owner));
        obj.insert("chosen".into(), Value::Object(chosen));
    }
    Ok(base)
}
fn routes_output(
    repo: Option<&Path>,
    has_git_repo: bool,
    cwd: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let selected_room = room(options);
    let base = routes_report(
        repo,
        has_git_repo,
        cwd,
        source,
        options,
        selected_room.as_deref(),
        services,
        stop,
    )?;
    if !options.has("json")
        && std::env::var_os("OMS_PANEL_READER_BOARD").is_some()
        && let Some(mut tty) = crate::input::TerminalInput::open()?
    {
        let report = json!({"kind":"oms-panel-routes","routes":base});
        crate::reader::page(repo.unwrap_or(cwd), &report, &mut tty, stop)?;
        return Ok((0, vec![], vec![]));
    }
    output(base)
}

/// Split `--to PROVIDER[:account=ID]` and merge `--account ID`.
fn route_target(options: &Options) -> Result<(Option<String>, Option<String>)> {
    let (provider, inline) = match options.get("to") {
        Some(to) => match to.split_once(":account=") {
            Some((p, a)) => {
                require(!p.is_empty(), "--to :account= requires a provider")?;
                (Some(p.to_owned()), Some(a.to_owned()))
            }
            None => (Some(to.to_owned()), None),
        },
        None => (None, None),
    };
    let flag = options.get("account").map(str::to_owned);
    if let (Some(a), Some(b)) = (&inline, &flag) {
        require(a == b, "--to :account= conflicts with --account")?;
    }
    let account = inline.or(flag);
    if let Some(id) = &account {
        require(
            !id.is_empty()
                && id.len() <= 64
                && (id.as_bytes()[0].is_ascii_lowercase() || id.as_bytes()[0].is_ascii_digit())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)),
            "invalid account id",
        )?;
        require(
            provider.is_some() || options.has("continue"),
            "--account requires --to PROVIDER or --continue",
        )?;
    }
    Ok((provider, account))
}

const ROUTE_RECENT_SECONDS: u64 = 6 * 60 * 60;
const ROUTE_IN_FLIGHT_STATES: &[&str] = &["starting", "working", "running", "verifying"];

fn routing_inputs(
    repo: Option<&Path>,
    cwd: Option<&Path>,
    room_id: Option<&str>,
    owner_hint: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<(routing::RoutingInputs, bool)> {
    let query_dir = repo.or(cwd).unwrap_or_else(|| Path::new("."));
    let raw = query(
        services,
        NativeCommand::ProviderCatalog,
        query_dir,
        &["--json"],
        stop,
    )?;
    let providers = raw.get("providers").unwrap_or(&raw);
    require(providers.is_array(), "provider catalog unavailable")?;
    let mut inputs = routing::RoutingInputs {
        installed: Some(Default::default()),
        owner_model: None,
        accounts: routing::Accounts::configured().ok().flatten(),
        ..Default::default()
    };
    for provider in rows(providers) {
        let name = text(provider, "provider");
        // `oms models --json` reports `present`/`usable` (usable null = unprobed); `installed` is the doctor-row spelling.
        if provider["installed"] == true
            || (provider["present"] == true && provider["usable"] != false)
        {
            inputs.installed.as_mut().unwrap().insert(name.into());
        }
        let strings = |key: &str| -> Vec<String> {
            rows(&provider[key])
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let models = if provider.get("catalog_models").is_some() {
            strings("catalog_models")
        } else {
            strings("models")
        };
        inputs.provider_evidence.insert(
            name.into(),
            routing::ProviderEvidence::from_catalog_row(provider)?,
        );
        inputs.catalogs.insert(name.into(), models);
    }

    let env_owner = std::env::var("OMS_PANEL_OWNER")
        .ok()
        .filter(|v| !v.is_empty());
    let owner_matches_env = match (env_owner.as_deref(), owner_hint) {
        (Some(eo), Some(oh)) => eo == oh,
        _ => true,
    };
    let env_main_attempt = owner_matches_env
        .then(|| std::env::var("OMS_PANEL_MAIN_ATTEMPT").ok())
        .flatten()
        .filter(|v| !v.is_empty());
    let env_room_participant = owner_matches_env
        .then(|| std::env::var("OMS_ROOM_PARTICIPANT").ok())
        .flatten()
        .filter(|v| !v.is_empty());

    let now = unix_now();
    let mut report_stub = Value::Null;
    let mut ambiguous_active_main = false;
    let mut events_observed = false;
    if let Some(repo_path) = repo {
        let attempts = query(
            services,
            NativeCommand::Events,
            repo_path,
            &["list", "--json"],
            stop,
        )
        .unwrap_or(Value::Null);
        events_observed = attempts.is_array();
        let room_rows = room_id.and_then(|id| {
            query(
                services,
                NativeCommand::Room,
                repo_path,
                &["show", "--id", id, "--json"],
                stop,
            )
            .ok()
        });
        if let Some(ref room_state) = room_rows {
            report_stub = json!({"room": room_state});
        }

        let mut terminal_main_ids = std::collections::BTreeSet::new();
        let mut active_mains: Vec<(String, String, String)> = Vec::new();
        let mut account_attempts = std::collections::BTreeMap::new();
        for row in rows(&attempts) {
            let room_match = room_id.is_none_or(|id| {
                row["refs"]["panel_room_id"].as_str() == Some(id)
                    || row["refs"]["panel_room"].as_str() == Some(id)
                    || row["panel"]["room_id"].as_str() == Some(id)
            });
            let prov = text(row, "provider");
            let state = text(row, "state");
            let refs = &row["refs"];
            let attempt_id = text(row, "attempt_id");
            let well_formed = !attempt_id.is_empty()
                && !prov.is_empty()
                && !state.is_empty()
                && refs.is_object()
                && ["panel_role", "panel_account", "panel_room_id", "panel_room"]
                    .iter()
                    .all(|key| refs.get(*key).is_none_or(Value::is_string))
                && row["panel"].get("room_id").is_none_or(Value::is_string);
            events_observed &= well_formed;
            let is_main = refs["panel_role"] == "main" || row["tool"] == "panel-main";
            let is_terminal =
                row["terminal"] == true || oms_panel_core::state::TERMINAL_STATES.contains(&state);
            if well_formed
                && room_match
                && !is_main
                && !text(refs, "panel_role").is_empty()
                && let Some(started) = metrics::stamp(&row["created_at"])
                    .filter(|started| *started > 0.0 && *started <= now)
            {
                if now - started <= ROUTE_RECENT_SECONDS as f64 {
                    *inputs.recent.entry(prov.to_owned()).or_insert(0) += 1;
                }
                if !is_terminal && ROUTE_IN_FLIGHT_STATES.contains(&state) {
                    *inputs.in_flight.entry(prov.to_owned()).or_insert(0) += 1;
                }
            }
            if is_main && is_terminal {
                for id in [
                    text(row, "attempt_id"),
                    text(&row["refs"], "panel_room_participant"),
                ] {
                    if !id.is_empty() {
                        terminal_main_ids.insert(id.to_owned());
                    }
                }
            }
            if room_match
                && is_main
                && !is_terminal
                && matches!(owner_hint, Some(o) if text(&row["refs"], "panel_owner") == o || prov == o)
            {
                active_mains.push((
                    text(row, "attempt_id").to_owned(),
                    text(&row["refs"], "panel_room_participant").to_owned(),
                    text(&row["refs"], "panel_model").to_owned(),
                ));
            }
            if !attempt_id.is_empty() {
                let seat = (well_formed
                    && !is_main
                    && !is_terminal
                    && matches!(state, "working" | "running")
                    && routing::ACCOUNT_PROVIDERS.contains(&prov)
                    && refs.get("panel_provider").is_none_or(|label| label == prov))
                .then(|| {
                    let recorded = text(refs, "panel_account");
                    let candidate = match recorded.split_once(':') {
                        Some((provider, alias)) if provider == prov => alias,
                        Some(_) => return None,
                        None => recorded,
                    };
                    crate::cli::valid_account_id(candidate).then_some((prov, candidate))
                })
                .flatten();
                account_attempts
                    .entry(attempt_id)
                    .and_modify(|entry| *entry = None)
                    .or_insert(seat);
            }
            if (prov == "pi" || text(&row["refs"], "panel_harness") == "pi")
                && (row["refs"]["daily_cap"] == true || text(row, "error").contains("daily"))
                && let Some(installed) = inputs.installed.as_mut()
            {
                installed.remove("pi");
            }
        }

        for (provider, alias) in account_attempts.into_values().flatten() {
            *inputs
                .running_accounts
                .entry(format!("{provider}:{alias}"))
                .or_insert(0) += 1;
        }
        let selected_active = env_main_attempt
            .as_deref()
            .and_then(|target| active_mains.iter().find(|(aid, _, _)| aid == target))
            .or_else(|| {
                env_room_participant.as_deref().and_then(|target| {
                    active_mains
                        .iter()
                        .find(|(aid, pid, _)| pid == target || aid == target)
                })
            })
            .or_else(|| {
                if active_mains.len() == 1 {
                    active_mains.last()
                } else if !active_mains.is_empty() {
                    let families: Option<Vec<&'static str>> = owner_hint.and_then(|o| {
                        active_mains
                            .iter()
                            .map(|(_, _, model)| {
                                if model.is_empty() {
                                    None
                                } else {
                                    routing::model_family(o, model).ok()
                                }
                            })
                            .collect()
                    });
                    if let Some(fams) = families
                        && let Some(first) = fams.first().copied()
                        && fams.iter().all(|f| *f == first)
                    {
                        active_mains.last()
                    } else {
                        ambiguous_active_main = true;
                        None
                    }
                } else {
                    None
                }
            });

        if let Some((aid, pid, model)) = selected_active {
            if !model.is_empty() {
                inputs.owner_model = Some(model.clone());
            } else if let Some(ref room_state) = room_rows {
                inputs.owner_model = rows(&room_state["participants"])
                    .iter()
                    .rev()
                    .find(|p| {
                        let p_id = text(p, "participant");
                        let p_att = text(p, "attempt");
                        p["role"] == "main"
                            && p["joined"] == true
                            && (!pid.is_empty() && p_id == pid
                                || (!aid.is_empty() && (p_id == aid || p_att == aid)))
                    })
                    .map(|p| text(p, "model").to_owned())
                    .filter(|m| !m.is_empty());
            }
        } else if !ambiguous_active_main
            && active_mains.is_empty()
            && let Some(ref room_state) = room_rows
        {
            let owner = owner_hint.unwrap_or_default();
            let participants = rows(&room_state["participants"]);
            let non_terminal_main = |p: &&Value| -> bool {
                let p_id = text(p, "participant");
                let p_att = text(p, "attempt");
                let p_att_id = text(p, "attempt_id");
                p["role"] == "main"
                    && p["joined"] == true
                    && (text(p, "provider") == owner || p_id == owner)
                    && ![p_id, p_att, p_att_id]
                        .into_iter()
                        .any(|id| !id.is_empty() && terminal_main_ids.contains(id))
            };
            let matched = env_room_participant
                .as_deref()
                .and_then(|target| {
                    participants
                        .iter()
                        .rev()
                        .find(|p| non_terminal_main(p) && text(p, "participant") == target)
                })
                .or_else(|| {
                    env_main_attempt.as_deref().and_then(|target| {
                        participants.iter().rev().find(|p| {
                            non_terminal_main(p)
                                && (text(p, "attempt") == target || text(p, "attempt_id") == target)
                        })
                    })
                })
                .or_else(|| participants.iter().rev().find(non_terminal_main));
            inputs.owner_model = matched
                .map(|p| text(p, "model").to_owned())
                .filter(|m| !m.is_empty());
        }
    }

    if inputs.owner_model.is_none() && !ambiguous_active_main {
        if owner_hint == Some("antigravity")
            && env_owner.as_deref().is_none_or(|eo| eo == "antigravity")
        {
            inputs.owner_model = crate::native_panel::detect_antigravity_model();
        } else if !matches!(owner_hint, Some("codex" | "claude"))
            && env_owner
                .as_deref()
                .is_some_and(|eo| Some(eo) == owner_hint)
        {
            inputs.owner_model = std::env::var("OMS_MODEL").ok().filter(|v| !v.is_empty());
        }
    }

    inputs.status = metrics::collect_local(repo, &report_stub, None, now);
    Ok((inputs, events_observed))
}

fn dispatch_command(
    repo: &Path,
    execution: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let policy = policy(source)?;
    let owner = owner(options);
    let mut role = options.text("dispatch", "worker");
    let mut access = options.text(
        "access",
        if options.has("continue") {
            "write"
        } else {
            "read"
        },
    );
    let mut purpose = options.get("purpose").map(str::to_owned);
    let mut scopes = options.all("scope");
    let mut verifier = options.get("verify").map(str::to_owned);
    let mut workload = options.get("workload").map(str::to_owned);
    let mut task = options
        .get("continue")
        .or_else(|| options.get("task-id"))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "panel-{}",
                &oms_panel_core::digest(
                    options
                        .get("prompt")
                        .or_else(|| options.get("brief-file"))
                        .unwrap_or("")
                        .as_bytes()
                )[..12]
            )
        });
    let attempts = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let main_participant = options
        .get("main")
        .map(str::to_owned)
        .or_else(|| std::env::var("OMS_ROOM_PARTICIPANT").ok());
    let main_attempt_id = if options.has("dry-run") {
        std::env::var("OMS_PANEL_MAIN_ATTEMPT")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| main_attempt(repo, options, services, stop).ok())
    } else {
        Some(main_attempt(repo, options, services, stop)?)
    };
    let mut continued_account = None;
    let mut continued_provider = None;
    if options.has("continue")
        && let Some(defaults) = dispatch::continuation_defaults(
            repo,
            &owner,
            main_participant.as_deref(),
            main_attempt_id.as_deref(),
            &task,
            room(options).as_deref(),
            services,
            stop,
        )?
    {
        if !options.has("dispatch") {
            role = defaults.role;
        }
        if !options.has("access") {
            access = "write".into();
        }
        if role == "worker" {
            purpose = purpose.or(defaults.purpose);
        }
        workload = workload.or(defaults.workload);
        verifier = verifier.or(defaults.verify);
        if !options.has("scope") {
            scopes = defaults.scopes;
        }
        continued_account = defaults.account;
        continued_provider = defaults.provider;
    }
    let (mut inputs, _) = routing_inputs(
        Some(repo),
        Some(execution),
        room(options).as_deref(),
        Some(&owner),
        services,
        stop,
    )?;
    let (target, flag_account) = route_target(options)?;
    // A continuation binding follows its recorded provider; an explicit `--to`
    // for another provider drops it, an explicit account always wins.
    let continued = continued_account.filter(|_| {
        flag_account.is_none()
            && target
                .as_deref()
                .is_none_or(|t| Some(t) == continued_provider.as_deref())
    });
    inputs.preferred = target
        .clone()
        .or_else(|| continued.as_ref().and(continued_provider.clone()));
    inputs.explicit_target = target.is_some();
    if (flag_account.is_some() || continued.is_some())
        && let Some(scope) = target.clone().or(continued_provider.clone())
        && let Some(installed) = inputs.installed.as_mut()
    {
        installed.retain(|p| *p == scope);
    }
    inputs.requested_account = flag_account.or(continued);
    inputs.requested_model = options.get("model").map(str::to_owned);
    inputs.requested_effort = options.get("reasoning-effort").map(str::to_owned);
    let route = routing::allocate(
        &policy,
        &owner,
        &role,
        workload.as_deref(),
        &access,
        purpose.as_deref(),
        &inputs,
    )?;
    require(
        inputs.requested_account.is_none() || route.account_pinned,
        "requested account is not configured for the routed provider",
    )?;
    if let Some(provider) = target.as_deref() {
        require(
            route.candidate.provider == provider,
            &format!(
                "requested provider '{provider}' unavailable for role '{role}' (allocated '{}')",
                route.candidate.provider
            ),
        )?;
    }
    routing::valid_route(&route.candidate)?;
    let mut request = dispatch::DispatchRequest {
        route,
        task_id: std::mem::take(&mut task),
        prompt: options.get("prompt").map(str::to_owned),
        brief: options.get("brief-file").map(|p| {
            let p = Path::new(p);
            if p.is_absolute() {
                p.into()
            } else {
                execution.join(p)
            }
        }),
        verify: verifier,
        scopes,
        room: room(options),
        main: main_participant,
        main_attempt: main_attempt_id,
        continue_task: options.get("continue").map(str::to_owned),
        base: None,
        carried_patch: None,
        allow_stale_base: options.has("allow-stale-base"),
        from_task: options.all("from-task"),
        resume_session: None,
        candidate: None,
        dry_run: options.has("dry-run"),
        execution_repo: execution.into(),
        label: options.get("label").map(str::to_owned),
        alias: options.get("alias").map(str::to_owned),
        no_web: options.has("no-web"),
    };
    if request.continue_task.is_some() {
        dispatch::continuation(repo, &mut request, &attempts, services, stop)?;
    }
    output(dispatch::dispatch(repo, &request, services, stop)?)
}
fn salvage_command(
    repo: &Path,
    execution: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let task = options.required("salvage")?;
    let selected = options.all("attempt");
    require(
        selected.len() <= 1,
        "salvage selects at most one stopped attempt",
    )?;
    let report = results::results(repo, Some(&task), services, stop)?;
    let calls: Vec<_> = rows(&report["rows"])
        .iter()
        .flat_map(|v| rows(&v["calls"]))
        .filter(|v| {
            v["kind"] == "delegate" && selected.first().is_none_or(|id| v["attempt_id"] == *id)
        })
        .collect();
    require(
        calls.len() == 1,
        "salvage candidate absent or ambiguous; select --attempt",
    )?;
    let call = calls[0];
    let attempts = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let attempt = rows(&attempts)
        .iter()
        .find(|v| v["attempt_id"] == call["attempt_id"])
        .ok_or_else(|| Error::new("salvage attempt missing"))?;
    let kept = salvage::recorded_kept(repo, attempt, Some(call))?;
    let base = call["base_sha"]
        .as_str()
        .or_else(|| attempt["refs"]["panel_base_sha"].as_str())
        .or_else(|| attempt["refs"]["base_sha"].as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::new("salvage base commit missing"))?;
    let result = services.execute(
        &oms_panel_core::services::ExecutionRequest::new(
            strings(&[
                "git",
                "rev-parse",
                "--verify",
                &format!("{base}^{{commit}}"),
            ]),
            execution,
        ),
        stop,
    )?;
    result.success()?;
    let base = String::from_utf8(result.stdout)
        .map_err(Error::new)?
        .trim()
        .to_owned();
    output(salvage::salvage(
        repo,
        &salvage::SalvageRequest {
            task_id: task,
            attempt_id: text(attempt, "attempt_id").into(),
            member: text(&attempt["refs"], "panel_room_participant").into(),
            kept,
            execution_repo: execution.into(),
            base,
        },
        services,
        stop,
    )?)
}
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn acceptance_output(request: &results::AcceptRequest, accepted: &Value) -> String {
    let execution = shell_quote(&request.execution_repo.to_string_lossy());
    let heading = if accepted["settlement_only"] == true {
        "accepted commit confirmed; publication pending"
    } else {
        "committed locally; publication pending"
    };
    let mut result = format!("{heading}: {}\n", text(accepted, "sha"));
    if let Some(retirement) = accepted.get("resource_retirement") {
        let status = text(retirement, "status");
        result.push_str(&format!("Accepted write custody: {status}\n"));
        let reason = crate::render::clean(text(retirement, "reason"), 240);
        if status == "unknown" && !reason.is_empty() {
            result.push_str(&format!("Reason: {reason}\n"));
        }
        if ["pending", "unknown"].contains(&status) {
            result.push_str(&format!("Retry settlement: oms panel --accept {} --repo {execution} --owner {} --message {} --verify {} --expected-patch-sha256 {}\n",
                shell_quote(&request.task_id), shell_quote(&request.owner),
                shell_quote(&request.message), shell_quote(&request.verify),
                shell_quote(text(accepted, "patch_sha256"))));
        }
    }
    result.push_str(&format!("Next: oms land --repo {execution} (opens a pull request; add --merge only when the user asked to merge)\nThen, once merged: oms panel --finalize --repo {execution} --landed-commit <full head SHA that oms land pushed>\n"));
    result
}

fn mail(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let id = options.required("mail")?;
    crate::identifier(&id)?;
    let room = options.required("room")?;
    let state = query(
        services,
        NativeCommand::Room,
        repo,
        &["show", "--id", &room, "--json"],
        stop,
    )?;
    require(
        rows(&state["participants"])
            .iter()
            .any(|v| v["participant"] == id && v["role"] == "main" && v["joined"] == true),
        "main left or its role changed; refresh before reading messages",
    )?;
    let mut lines = vec![format!("Messages · {id}")];
    for message in rows(&state["messages"]).iter().filter(|v| {
        v["recipient"] == id
            || v["recipient"] == "all"
            || rows(&v["targets"])
                .iter()
                .any(|v| v == &Value::String(id.clone()))
    }) {
        lines.push(crate::render::clean(
            &format!(
                "{} {} → {} ({})",
                text(message, "ts"),
                text(message, "sender"),
                id,
                text(message, "message_kind")
            ),
            200,
        ));
        lines.extend(crate::render::wrap(
            text(message, "text"),
            crate::layout::dimensions().0,
        ));
    }
    plain(lines.join("\n") + "\n")
}
fn find_recent_native_session(
    provider: &str,
    consumer: &str,
    explicit_session: &str,
) -> Option<String> {
    if !explicit_session.is_empty()
        && !explicit_session.contains('/')
        && !explicit_session.contains("..")
    {
        return Some(explicit_session.to_owned());
    }
    if consumer.is_empty() {
        return None;
    }
    let dirs: Vec<PathBuf> = match provider {
        "codex" => {
            let base = std::env::var_os("CODEX_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .filter(|v| !v.is_empty())
                        .map(|h| PathBuf::from(h).join(".codex"))
                })?;
            vec![base.join("sessions")]
        }
        "claude" => {
            let base = std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .filter(|v| !v.is_empty())
                        .map(|h| PathBuf::from(h).join(".claude"))
                })?;
            vec![base.join("projects")]
        }
        _ => return None,
    };

    let mut stack = dirs;
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        if visited >= 256 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort_by(|a, b| b.cmp(a));
        for path in entries {
            if visited >= 256 {
                break;
            }
            visited += 1;
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            use std::os::unix::fs::MetadataExt;
            if meta.nlink() != 1 {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(".jsonl") {
                continue;
            }
            let flags = rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC;
            let Ok(fd) = rustix::fs::open(&path, flags, rustix::fs::Mode::empty()) else {
                continue;
            };
            let mut file = std::fs::File::from(fd);
            use std::io::Read;
            let mut head = vec![0u8; 4096];
            let Ok(n) = file.read(&mut head) else {
                continue;
            };
            head.truncate(n);
            let snippet = String::from_utf8_lossy(&head);
            if snippet.contains(consumer) || name.contains(consumer) {
                let stem = name.trim_end_matches(".jsonl");
                if provider == "codex"
                    && stem.starts_with("rollout-")
                    && let Some(uuid) = extract_trailing_uuid(stem)
                {
                    return Some(uuid);
                }
                if let Some(sid) = snippet.lines().find_map(|line| {
                    serde_json::from_str::<Value>(line).ok().and_then(|v| {
                        v["session_id"]
                            .as_str()
                            .or_else(|| v["payload"]["id"].as_str())
                            .or_else(|| v["sessionId"].as_str())
                            .map(str::to_owned)
                    })
                }) {
                    return Some(sid);
                }
                return Some(stem.to_owned());
            }
        }
    }
    None
}

fn extract_trailing_uuid(stem: &str) -> Option<String> {
    if stem.len() >= 36 {
        let candidate = &stem[stem.len() - 36..];
        let parts: Vec<&str> = candidate.split('-').collect();
        if parts.len() == 5
            && [8, 4, 4, 4, 12]
                .iter()
                .zip(&parts)
                .all(|(len, p)| p.len() == *len && p.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

fn canonical_match(a: &str, repo: &Path) -> bool {
    if a.is_empty() {
        return false;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(repo)) {
        (Ok(ca), Ok(cr)) => ca == cr,
        _ => Path::new(a) == repo,
    }
}

fn chats(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    parent_only()?;
    let id = room(options).ok_or_else(|| Error::new("select an existing room first"))?;
    let state = query(
        services,
        NativeCommand::Room,
        repo,
        &["show", "--id", &id, "--json"],
        stop,
    )?;
    let all = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )
    .unwrap_or(Value::Null);

    let mut live_panes: std::collections::BTreeSet<(String, String, String)> = Default::default();
    if let Ok(out) = terminals::tmux(
        services,
        repo,
        &strings(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{window_id}\t#{session_id}",
        ]),
        stop,
    ) {
        for line in out.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() >= 3 && valid_target(cols[0], '%') {
                live_panes.insert((cols[0].into(), cols[1].into(), cols[2].into()));
            }
        }
    }

    let mut tmux_windows: std::collections::BTreeMap<String, (String, String, String)> =
        Default::default();
    if let Ok(out) = terminals::tmux(
        services,
        repo,
        &strings(&[
            "list-windows",
            "-a",
            "-F",
            "#{window_id}\t#{@oms_panel_room_participant}\t#{@oms_panel_repo}\t#{@oms_panel_native_pane}\t#{@oms_panel_room}\t#{session_id}",
        ]),
        stop,
    ) {
        for line in out.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 4 {
                continue;
            }
            let win = cols[0];
            let part = cols[1];
            let wrepo = cols[2];
            let pane = cols[3];
            let wroom = cols.get(4).copied().unwrap_or("");
            let sess = cols.get(5).copied().unwrap_or("");
            if !canonical_match(wrepo, repo) {
                continue;
            }
            if !wroom.is_empty() && wroom != id {
                continue;
            }
            if valid_target(win, '@') && valid_target(pane, '%') {
                let pane_ok = live_panes.iter().any(|(p, w, _)| p == pane && w == win);
                if pane_ok {
                    let eff_sess = if valid_target(sess, '$') {
                        sess.to_owned()
                    } else {
                        live_panes
                            .iter()
                            .find(|(p, w, _)| p == pane && w == win)
                            .map(|(_, _, s)| s.clone())
                            .unwrap_or_default()
                    };
                    tmux_windows
                        .insert(part.to_owned(), (eff_sess, win.to_owned(), pane.to_owned()));
                }
            }
        }
    }

    type PanelRow = (Value, Option<(String, String, String)>);
    let mut built_rows: Vec<PanelRow> = Vec::new();
    for member in rows(&state["participants"]) {
        let participant = text(member, "participant");
        let label = text(member, "label");
        let provider = text(member, "provider");
        let role = text(member, "role");
        let joined = member["joined"].as_bool().unwrap_or(false);
        let attempt = rows(&all).iter().rev().find(|v| {
            (!member["attempt"].is_null() && v["attempt_id"] == member["attempt"])
                || v["refs"]["panel_room_participant"] == participant
                || (role == "main"
                    && v["tool"] == "panel-main"
                    && state::main_participant(v) == participant)
        });

        let mut win_target = tmux_windows.get(participant).cloned();
        if win_target.is_none()
            && let Some(att) = attempt
        {
            let p = text(&att["refs"], "panel_native_pane");
            if let Some((_, w, s)) = live_panes.iter().find(|(lp, _, _)| lp == p) {
                win_target = Some((s.clone(), w.clone(), p.to_owned()));
            }
        }

        let consumer = member["consumer"]
            .as_str()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                attempt.and_then(|a| a["refs"]["consumer"].as_str().filter(|v| !v.is_empty()))
            })
            .unwrap_or("");
        let explicit_session = attempt
            .and_then(|a| {
                a["refs"]["panel_native_session"]
                    .as_str()
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or("");
        let session_id = find_recent_native_session(provider, consumer, explicit_session);
        let uri = session_id.as_ref().and_then(|sid| {
            if provider == "codex" {
                Some(format!("codex://threads/{sid}"))
            } else {
                None
            }
        });
        let terminal = win_target.is_some();
        let (status, reason): (&str, Option<&str>) = if terminal {
            ("live", None)
        } else if session_id.is_some() {
            ("available", None)
        } else if consumer.is_empty() && explicit_session.is_empty() {
            (
                "unresolved",
                Some("no native session recorded for this main"),
            )
        } else {
            (
                "unresolved",
                Some(
                    "session not found in the recent local history (older sessions are not scanned)",
                ),
            )
        };
        let title = if label.is_empty() {
            format!("{provider} {role} (no task title)")
        } else {
            label.to_owned()
        };
        built_rows.push((
            json!({
                "participant": participant,
                "label": label,
                "provider": provider,
                "role": role,
                "joined": joined,
                "session_id": session_id,
                "uri": uri,
                "terminal": terminal,
                "status": status,
                "reason": reason,
                "title": title,
            }),
            win_target,
        ));
    }

    if let Some(participant) = options.get("open-chat") {
        let (selected, win_target) = built_rows
            .iter()
            .find(|(r, _)| r["participant"] == participant)
            .ok_or_else(|| Error::new("chat participant unavailable"))?;
        let requested = options.text("chat-surface", "auto");
        let use_terminal = match requested.as_str() {
            "terminal" => true,
            "app" => false,
            _ => win_target.is_some(),
        };
        let dry_run = options.has("dry-run");
        if use_terminal {
            let (sess, win, pane) = win_target.as_ref().ok_or_else(|| {
                Error::new("chat terminal binding unavailable; session was preserved")
            })?;
            let cmds = terminals::switch_commands(services, repo, sess, win, pane, stop)?;
            if !dry_run {
                for argv in &cmds {
                    terminals::tmux(services, repo, argv, stop)
                        .map_err(|_| Error::new("tmux navigation failed; session was preserved"))?;
                }
            }
            return output(json!({
                "schema": 1,
                "kind": "oms-panel-chat-open",
                "participant": participant,
                "surface": "terminal",
                "dry_run": dry_run,
                "pane": pane,
                "session": sess,
                "window": win,
            }));
        }

        let uri = selected["uri"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                Error::new(
                    selected["reason"]
                        .as_str()
                        .unwrap_or("native app URI unavailable for participant"),
                )
            })?;
        let scheme = uri.split(':').next().unwrap_or("");
        if !dry_run {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                if !scheme.is_empty() {
                    let mime_res = services.execute(
                        &oms_panel_core::services::ExecutionRequest::new(
                            strings(&[
                                "xdg-mime",
                                "query",
                                "default",
                                &format!("x-scheme-handler/{scheme}"),
                            ]),
                            repo,
                        ),
                        stop,
                    )?;
                    mime_res.success()?;
                }
                "xdg-open"
            };
            let res = services.execute(
                &oms_panel_core::services::ExecutionRequest::new(strings(&[opener, uri]), repo),
                stop,
            )?;
            res.success()?;
        }
        return output(json!({
            "schema": 1,
            "kind": "oms-panel-chat-open",
            "participant": participant,
            "surface": "app",
            "dry_run": dry_run,
            "uri": uri,
            "ui_observed": false,
        }));
    }

    let chat_rows: Vec<Value> = built_rows.into_iter().map(|(r, _)| r).collect();
    let authority = state.get("authority").cloned().unwrap_or(Value::Null);
    let report = json!({
        "schema": 1,
        "kind": "oms-panel-chats",
        "room": id,
        "rows": chat_rows,
        "authority": authority,
    });
    if options.has("json") {
        output(report)
    } else {
        plain(crate::render::value_lines(&report, crate::layout::dimensions().0).join("\n") + "\n")
    }
}
pub(crate) fn valid_target(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|v| !v.is_empty() && v.len() <= 20 && v.bytes().all(|v| v.is_ascii_digit()))
}

fn sessions(report: &Value, options: &Options) -> Result<Output> {
    let mut sessions = Vec::new();
    let min_age = options
        .get("min-age")
        .map(oms_panel_core::metrics::duration)
        .transpose()?;
    for call in crate::render::calls(report) {
        let raw = text(&call, "state");
        let (state, waiting) = match raw {
            "queued" | "idle" => ("waiting", "none"),
            "starting" | "working" | "running" | "verifying" | "manual" | "live marker" => {
                ("working", "none")
            }
            "waiting_input" => ("waiting", "input"),
            "waiting_approval" => ("waiting", "approval"),
            "review" => ("waiting", "review"),
            "blocked" => ("blocked", "owner"),
            "done" | "exited" | "cancelled" => ("done", "none"),
            "failed" | "timed_out" | "abandoned" => ("failed", "none"),
            _ => ("unknown", "unknown"),
        };
        if options.get("state").is_some_and(|v| v != state)
            || options
                .get("provider")
                .is_some_and(|v| call["provider"] != v)
            || options.get("role").is_some_and(|v| call["role"] != v)
        {
            continue;
        }
        let ended = [
            "done",
            "exited",
            "cancelled",
            "failed",
            "timed_out",
            "abandoned",
        ]
        .contains(&raw);
        let since = if ended {
            call["updated_at"].as_str()
        } else {
            None
        };
        if min_age.is_some_and(|age| {
            since
                .and_then(|v| oms_panel_core::metrics::stamp(&Value::String(v.into())))
                .is_none_or(|at| oms_panel_core::timestamp() as f64 - at < age)
        }) {
            continue;
        }
        sessions.push(json!({"id":call["attempt"],"attempt":call["attempt"],"participant":call["participant"],"provider":call["provider"],"role":call["role"],"state":state,"waiting_for":waiting,"raw_state":raw,"since":since,"since_basis":if since.is_some(){Some("last event")}else{None},"source":"attempt","stale":report["capacity_stale"]==true}));
    }
    let stale = report["capacity_stale"] == true || report["collection"]["ok"] != true;
    let reason = stale.then_some("held previous read");
    if options.count_rows {
        return if options.has("json") {
            output(
                json!({"schema":1,"kind":"oms-sessions-count","count":sessions.len(),"stale":stale,"stale_reason":reason}),
            )
        } else {
            plain(format!("{}\n", sessions.len()))
        };
    }
    if options.has("json") {
        output(
            json!({"schema":1,"kind":"oms-sessions","stale":stale,"stale_reason":reason,"sessions":sessions}),
        )
    } else {
        plain(
            sessions
                .iter()
                .map(|v| {
                    crate::render::clean(
                        &format!(
                            "{} {} {} {} {}",
                            text(v, "id"),
                            text(v, "provider"),
                            text(v, "role"),
                            text(v, "state"),
                            text(v, "waiting_for")
                        ),
                        300,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
    }
}
fn summarize_review(telemetry_rows: &[Value], task: &str, patch_sha: Option<&str>) -> Value {
    let Some(sha) = patch_sha.filter(|v| !v.is_empty()) else {
        return json!({"status":"unrecorded","seats":{},"exit":null,"gate_verify_exit":null});
    };
    let found = telemetry_rows.iter().rev().find(|row| {
        row["kind"] == "review-outcome"
            && row["task_id"] == task
            && row["review"]["candidate"]["patch_sha256"] == sha
    });
    let Some(row) = found else {
        return json!({"status":"unrecorded","seats":{},"exit":null,"gate_verify_exit":null});
    };
    let mut seat_counts: std::collections::BTreeMap<String, usize> = Default::default();
    let seats = rows(&row["review"]["seats"]);
    for seat in seats {
        let verdict = seat["verdict"].as_str().unwrap_or("unknown").to_owned();
        *seat_counts.entry(verdict).or_insert(0) += 1;
    }
    let exit = row["exit"].as_i64();
    let gate_exit = row["review"]["gate_verify_exit"].as_i64();
    let status = if exit == Some(0)
        && gate_exit == Some(0)
        && !seats.is_empty()
        && seats.iter().all(|s| s["verdict"] == "pass")
    {
        "passed"
    } else if exit.is_some() || gate_exit.is_some() {
        "failed"
    } else {
        "unrecorded"
    };
    json!({
        "status": status,
        "seats": seat_counts,
        "exit": exit,
        "gate_verify_exit": gate_exit,
    })
}

fn attempt_duration(attempt: &Value) -> Value {
    if attempt["usage_reports"]["duration_ms"]
        .as_u64()
        .is_some_and(|c| c > 0)
        && let Some(ms) = attempt["usage"]["duration_ms"].as_f64()
        && ms.is_finite()
        && ms >= 0.0
    {
        return json!((ms / 1000.0).round() as i64);
    }
    let state = text(attempt, "state");
    if state == "working" || state == "running" {
        return Value::Null;
    }
    let start = metrics::stamp(&attempt["created_at"]);
    let end =
        metrics::stamp(&attempt["ended_at"]).or_else(|| metrics::stamp(&attempt["updated_at"]));
    match (start, end) {
        (Some(s), Some(e)) if e >= s => json!((e - s).round() as i64),
        _ => Value::Null,
    }
}

fn render_compare(result: &Value, width: usize) -> String {
    let attempts = rows(&result["attempts"]);
    let task = result["task_id"].as_str().unwrap_or("");
    let mut lines = vec![format!(
        "Compare task {task} ({} attempt(s); creation time, not a ranking):",
        attempts.len()
    )];
    let col_line = |label: &str, vals: Vec<String>| -> String {
        let mut s = format!("{label:<10}");
        for v in vals {
            s.push_str(&format!("  {v:<32}"));
        }
        crate::render::clean(&s, width)
    };
    if width >= 100 && !attempts.is_empty() {
        lines.push(col_line(
            "attempt",
            attempts
                .iter()
                .map(|a| format!("#{} {}", a["index"], a["attempt_id"].as_str().unwrap_or("")))
                .collect(),
        ));
        lines.push(col_line(
            "model",
            attempts
                .iter()
                .map(|a| {
                    format!(
                        "{}/{} {}",
                        a["provider"].as_str().unwrap_or(""),
                        a["model"].as_str().unwrap_or("unrecorded"),
                        a["effort"].as_str().unwrap_or("")
                    )
                })
                .collect(),
        ));
        lines.push(col_line(
            "verify",
            attempts
                .iter()
                .map(|a| {
                    a["verify"]["status"]
                        .as_str()
                        .unwrap_or("unrecorded")
                        .into()
                })
                .collect(),
        ));
        lines.push(col_line(
            "review",
            attempts
                .iter()
                .map(|a| {
                    a["review"]["status"]
                        .as_str()
                        .unwrap_or("unrecorded")
                        .into()
                })
                .collect(),
        ));
        lines.push(col_line(
            "changes",
            attempts
                .iter()
                .map(|a| {
                    if a["changes"]["files"].is_u64() {
                        format!(
                            "{}f +{} -{}",
                            a["changes"]["files"], a["changes"]["added"], a["changes"]["removed"]
                        )
                    } else if let Some(issue) = a["changes"]["issue"].as_str() {
                        issue.to_string()
                    } else {
                        "unrecorded".to_string()
                    }
                })
                .collect(),
        ));
        lines.push(col_line(
            "tokens",
            attempts
                .iter()
                .map(|a| {
                    a["tokens"]
                        .as_u64()
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "unrecorded".into())
                })
                .collect(),
        ));
    } else {
        for a in attempts {
            lines.push(crate::render::clean(
                &format!(
                    "#{} {} ({}/{})",
                    a["index"],
                    a["attempt_id"].as_str().unwrap_or(""),
                    a["provider"].as_str().unwrap_or(""),
                    a["model"].as_str().unwrap_or("unrecorded")
                ),
                width,
            ));
            lines.push(crate::render::clean(
                &format!(
                    "  verify     {}",
                    a["verify"]["status"].as_str().unwrap_or("unrecorded")
                ),
                width,
            ));
            lines.push(crate::render::clean(
                &format!(
                    "  review     {}",
                    a["review"]["status"].as_str().unwrap_or("unrecorded")
                ),
                width,
            ));
        }
    }
    lines.push(crate::render::clean(
        "Order is creation time, not a ranking.",
        width,
    ));
    lines.join("\n") + "\n"
}

fn comparison(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let task = options.required("compare")?;
    let report = results::results(repo, Some(&task), services, stop)?;
    let attempts = query(
        services,
        NativeCommand::Events,
        repo,
        &["list", "--json"],
        stop,
    )?;
    let telemetry = query(
        services,
        NativeCommand::Telemetry,
        repo,
        &["list", "--json"],
        stop,
    )
    .unwrap_or(Value::Null);
    let telemetry_rows = rows(&telemetry);
    let mut chosen: Vec<_> = rows(&attempts)
        .iter()
        .filter(|v| v["task_id"] == task && v["refs"]["panel_access"] == "write")
        .collect();
    require(!chosen.is_empty(), "no write attempts found for task")?;
    chosen.sort_by_key(|v| (text(v, "created_at"), text(v, "attempt_id")));
    let store = Store::open(repo)?;

    let mut path_sets: Vec<Option<std::collections::BTreeSet<String>>> = Vec::new();
    let mut partial = Vec::new();
    for (index, attempt) in chosen.iter().enumerate() {
        let call = rows(&report["rows"])
            .iter()
            .flat_map(|v| rows(&v["calls"]))
            .rfind(|v| v["attempt_id"] == attempt["attempt_id"] && v["kind"] == "delegate");
        let (changes, path_set) = match call.filter(|v| v["patch"].is_string()) {
            None => (Value::Null, None),
            Some(c) => match results::verified_patch(&store, c)
                .and_then(|p| oms_panel_core::files::patch_changes(&p))
            {
                Ok(list) => {
                    let files = list.len();
                    let added: usize = list.iter().map(|v| v.added).sum();
                    let removed: usize = list.iter().map(|v| v.removed).sum();
                    let set: std::collections::BTreeSet<String> =
                        list.into_iter().map(|v| v.path).collect();
                    (
                        json!({"files": files, "added": added, "removed": removed}),
                        Some(set),
                    )
                }
                Err(_) => (json!({"issue": "patch evidence unavailable"}), None),
            },
        };
        path_sets.push(path_set);
        let refs = &attempt["refs"];
        let patch_sha = call.and_then(|v| v["patch_sha256"].as_str());
        let tokens = if attempt["usage_reports"]["tokens"]
            .as_u64()
            .is_some_and(|c| c > 0)
        {
            attempt["usage"]["tokens"].clone()
        } else {
            call.and_then(|v| v["tokens"].as_u64())
                .map(Value::from)
                .unwrap_or(Value::Null)
        };
        partial.push(json!({
            "index": index + 1,
            "attempt_id": attempt["attempt_id"],
            "provider": attempt["provider"],
            "model": refs["panel_model"],
            "effort": refs["panel_effort"],
            "state": attempt["state"],
            "verify": {
                "status": call
                    .and_then(|v| v["verify_exit"].as_i64())
                    .map(|v| if v == 0 { "passed" } else { "failed" })
                    .unwrap_or("unrecorded"),
                "exit": call.and_then(|v| v["verify_exit"].as_i64()),
            },
            "review": summarize_review(telemetry_rows, &task, patch_sha),
            "changes": changes,
            "tokens": tokens,
            "duration_s": attempt_duration(attempt),
        }));
    }

    let entries: Vec<Value> = partial
        .into_iter()
        .enumerate()
        .map(|(i, mut entry)| {
            let has_other = path_sets
                .iter()
                .enumerate()
                .any(|(j, s)| j != i && s.is_some());
            if let (Some(mine), true) = (&path_sets[i], has_other) {
                let mut others = std::collections::BTreeSet::new();
                for (j, set_opt) in path_sets.iter().enumerate() {
                    if j != i
                        && let Some(set) = set_opt
                    {
                        others.extend(set.iter().cloned());
                    }
                }
                let only_all: Vec<String> = mine.difference(&others).cloned().collect();
                let shared_all: Vec<String> = mine.intersection(&others).cloned().collect();
                let only_here: Vec<String> = only_all
                    .iter()
                    .take(50)
                    .map(|p| crate::render::clean(p, 160))
                    .collect();
                let shared: Vec<String> = shared_all
                    .iter()
                    .take(50)
                    .map(|p| crate::render::clean(p, 160))
                    .collect();
                entry["only_here"] = json!(only_here);
                entry["shared"] = json!(shared);
                entry["only_count"] = json!(only_all.len());
                entry["shared_count"] = json!(shared_all.len());
            } else {
                entry["only_here"] = Value::Null;
                entry["shared"] = Value::Null;
                entry["only_count"] = Value::Null;
                entry["shared_count"] = Value::Null;
            }
            entry
        })
        .collect();

    let result = json!({
        "schema": 1,
        "kind": "oms-panel-compare",
        "task_id": task,
        "ranked": false,
        "order": "creation time",
        "attempts": entries,
        "notes": ["Order is creation time, not a ranking. Read-only: this view neither verifies nor accepts an attempt; use the existing admission path."]
    });
    if options.has("json") {
        output(result)
    } else {
        plain(render_compare(&result, crate::layout::dimensions().0))
    }
}

fn next_attention(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    type BoardEntry = (PathBuf, Option<String>, Option<(String, String, String)>);
    let mut boards: Vec<BoardEntry> = Vec::new();
    let mut seen: std::collections::BTreeSet<(PathBuf, Option<String>)> = Default::default();
    if let Ok(out) = terminals::tmux(
        services,
        repo,
        &strings(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_id}\t#{session_id}\t#{pane_id}\t#{@oms_panel_repo}\t#{@oms_panel_room}",
        ]),
        stop,
    ) {
        for line in out.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 4 {
                continue;
            }
            let win = cols[0];
            let sess = cols[1];
            let pane = cols[2];
            let brepo = cols[3];
            let broom = cols
                .get(4)
                .copied()
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            if brepo.is_empty() {
                continue;
            }
            if let Ok(canonical) = state::repository_for(Path::new(brepo), true)
                && seen.insert((canonical.clone(), broom.clone()))
            {
                let target =
                    if valid_target(sess, '$') && valid_target(win, '@') && valid_target(pane, '%')
                    {
                        Some((sess.to_owned(), win.to_owned(), pane.to_owned()))
                    } else {
                        None
                    };
                boards.push((canonical, broom, target));
            }
        }
    }
    if boards.is_empty() {
        boards.push((repo.to_path_buf(), room(options), None));
    }

    type AttentionChoice = (
        String,
        String,
        String,
        String,
        PathBuf,
        Option<(String, String, String)>,
        usize,
    );
    let mut chosen: Option<AttentionChoice> = None;
    for (board_repo, board_room, target) in boards {
        let board_state = match state::snapshot(&board_repo, board_room.as_deref(), services, stop)
        {
            Ok(s) => s,
            Err(_) => continue,
        };
        let room_key = board_room
            .as_deref()
            .or_else(|| board_state.report["room"]["id"].as_str())
            .unwrap_or("");
        let read = crate::attention::load(&board_repo, room_key);
        let items = crate::attention::items(&board_state.report, &read);
        let actionable: Vec<_> = items.into_iter().filter(|v| v.cls != "info").collect();
        let unread_count = actionable.len();
        if let Some(best) = actionable.into_iter().min_by_key(|v| {
            (
                if v.stamp.is_empty() {
                    "~".to_owned()
                } else {
                    v.stamp.clone()
                },
                crate::attention::CLASSES.iter().position(|c| *c == v.cls),
            )
        }) {
            let candidate_key = (
                if best.stamp.is_empty() {
                    "~".to_owned()
                } else {
                    best.stamp.clone()
                },
                crate::attention::CLASSES
                    .iter()
                    .position(|c| *c == best.cls),
            );
            let better = chosen.as_ref().is_none_or(|prev| {
                (
                    if prev.0.is_empty() {
                        "~".to_owned()
                    } else {
                        prev.0.clone()
                    },
                    crate::attention::CLASSES.iter().position(|c| *c == prev.1),
                ) > candidate_key
            });
            if better {
                chosen = Some((
                    best.stamp,
                    best.cls,
                    best.who,
                    best.what,
                    board_repo,
                    target,
                    unread_count,
                ));
            }
        }
    }

    if let Some((_, cls, who, what, board_repo, target, unread_count)) = chosen {
        if options.has("switch") {
            let (sess, win, pane) = target.ok_or_else(|| {
                Error::new("no verified tmux window holds this board; nothing to switch to")
            })?;
            let cmds = terminals::switch_commands(services, &board_repo, &sess, &win, &pane, stop)?;
            for argv in &cmds {
                terminals::tmux(services, &board_repo, argv, stop)?;
            }
        }
        let line = crate::render::clean(
            &format!(
                "{}: {} {} | {}",
                cls,
                who,
                what,
                board_repo.file_name().unwrap_or_default().to_string_lossy()
            ),
            300,
        );
        let stderr = format!("unread attention: {unread_count} item(s)\n").into_bytes();
        Ok((0, format!("{line}\n").into_bytes(), stderr))
    } else {
        plain("nothing waiting\n".into())
    }
}
#[cfg(test)]
mod tests {
    use super::{Session, acceptance_output, snapshot_with};
    use crate::{Result, Services, cli::Options};
    use oms_panel_core::{
        NativeCommand, Store, results,
        services::{CommandOutput, ExecutionRequest, PeerExecutor},
    };
    use serde_json::json;
    use std::ffi::OsString;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    struct Attempts;
    impl PeerExecutor for Attempts {
        fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
            Err(oms_panel_core::Error::new("unexpected execution"))
        }
    }
    impl Services for Attempts {
        fn command(
            &self,
            command: NativeCommand,
            _: &Path,
            _: &[OsString],
            _: &AtomicBool,
        ) -> Result<CommandOutput> {
            // A room selected by the ambient environment is answered as empty.
            let body = match command {
                NativeCommand::Events => json!([{"attempt_id":"worker-1","tool":"peer-delegate",
                    "provider":"codex","state":"done","terminal":true,"task_id":"task-1","refs":{}}]),
                NativeCommand::Room => json!({}),
                _ => return Err(oms_panel_core::Error::new("unexpected native command")),
            };
            Ok(CommandOutput {
                stdout: serde_json::to_vec(&body).unwrap(),
                cleanup_complete: true,
                output_complete: true,
                ..Default::default()
            })
        }
    }
    struct TempRepo(PathBuf);
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn store_file(repo: &Path, path: &str, bytes: &[u8]) {
        Store::open(repo)
            .unwrap()
            .lock_at(Path::new("lock"), Duration::from_secs(3))
            .unwrap()
            .write(Path::new(path), bytes, false, 1024 * 1024)
            .unwrap();
    }

    #[test]
    fn acceptance_custody_output_preserves_exact_retry_arguments() {
        let request = results::AcceptRequest {
            owner: "claude".into(),
            task_id: "task-1".into(),
            main_attempt_id: "main-1".into(),
            message: "fix: owner's $(false) change".into(),
            verify: "test \"$status\" = 'ready'".into(),
            execution_repo: PathBuf::from("/tmp/owner's repo"),
            expected_patch_sha256: None,
            allow_verifier_change: false,
            allow_floor_change: None,
        };
        let digest = "ab".repeat(32);
        let mut accepted =
            json!({"sha":"accepted-commit", "patch_sha256":digest, "settlement_only":true});
        assert!(!acceptance_output(&request, &accepted).contains("Accepted write custody"));
        for status in ["pending", "unknown", "retired", "not_reserved"] {
            accepted["resource_retirement"] = json!({"status":status});
            if status == "unknown" {
                accepted["resource_retirement"]["reason"] =
                    "custody changed\n \u{1b}[31mstop\u{1b}[0m".into();
            }
            let shown = acceptance_output(&request, &accepted);
            assert!(
                shown.contains("accepted commit confirmed; publication pending: accepted-commit")
            );
            assert!(shown.contains(&format!("Accepted write custody: {status}")));
            if status == "unknown" {
                assert!(shown.contains("Reason: custody changed stop\n"));
                assert!(!shown.contains('\u{1b}'));
            }
            let retry = shown
                .lines()
                .find_map(|line| line.strip_prefix("Retry settlement: "));
            if ["pending", "unknown"].contains(&status) {
                assert!(shown.find("Retry settlement:") < shown.find("Next: oms land"));
                // Parse the displayed command without invoking oms. Embedded
                // substitutions and quotes must remain literal argv bytes.
                let parsed = std::process::Command::new("sh")
                    .args([
                        "-c",
                        &format!("set -- {}; printf '%s\\0' \"$@\"", retry.unwrap()),
                    ])
                    .output()
                    .unwrap();
                assert!(parsed.status.success());
                let words = parsed
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|word| !word.is_empty())
                    .map(|word| std::str::from_utf8(word).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(
                    words,
                    [
                        "oms",
                        "panel",
                        "--accept",
                        "task-1",
                        "--repo",
                        "/tmp/owner's repo",
                        "--owner",
                        "claude",
                        "--message",
                        &request.message,
                        "--verify",
                        &request.verify,
                        "--expected-patch-sha256",
                        &digest
                    ]
                );
            } else {
                assert!(retry.is_none());
            }
        }
    }

    #[test]
    fn attempt_cache_tracks_ledger_generation_and_failures() {
        use std::cell::RefCell;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counting {
            calls: AtomicUsize,
            fail: AtomicBool,
            rows: RefCell<Option<serde_json::Value>>,
        }
        impl PeerExecutor for Counting {
            fn execute(&self, _: &ExecutionRequest, _: &AtomicBool) -> Result<CommandOutput> {
                Err(oms_panel_core::Error::new("unexpected execution"))
            }
        }
        impl Services for Counting {
            fn command(
                &self,
                command: NativeCommand,
                _: &Path,
                _: &[OsString],
                _: &AtomicBool,
            ) -> Result<CommandOutput> {
                if command != NativeCommand::Events {
                    return Err(oms_panel_core::Error::new("unexpected command"));
                }
                let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                if self.fail.load(Ordering::SeqCst) {
                    return Err(oms_panel_core::Error::new("query failed"));
                }
                let rows = self
                    .rows
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| json!([{"attempt_id": format!("attempt-{n}")}]));
                Ok(CommandOutput {
                    stdout: serde_json::to_vec(&rows).unwrap(),
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }
        let dir = std::env::temp_dir().join(format!("oms-panel-cache-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let repo = TempRepo(dir);
        let ledger = repo.0.join(".oms/lifecycle/events.jsonl");
        std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        std::fs::write(&ledger, "first").unwrap();
        let service = Counting {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            rows: RefCell::new(None),
        };
        let cache = oms_panel_core::state::AttemptCache::default();
        let stop = AtomicBool::new(false);
        let refresh = || {
            oms_panel_core::state::snapshot_with_cache(&repo.0, None, &service, &stop, Some(&cache))
        };
        refresh().unwrap();
        refresh().unwrap();
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);
        let alias = repo.0.join("alias");
        std::os::unix::fs::symlink(&repo.0, &alias).unwrap();
        assert_eq!(
            cache.current_attempt(&alias, "attempt-1", &stop),
            Some(json!({"attempt_id":"attempt-1"}))
        );
        let foreign = repo.0.join("foreign");
        std::fs::create_dir_all(foreign.join(".oms/lifecycle")).unwrap();
        std::fs::write(foreign.join(".oms/lifecycle/events.jsonl"), "first").unwrap();
        assert!(
            cache
                .current_attempt(&foreign, "attempt-1", &stop)
                .is_none()
        );
        assert!(cache.current_attempt(&repo.0, "attempt", &stop).is_none());
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);

        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap()
            .write_all(b"more")
            .unwrap();
        assert!(cache.current_attempt(&repo.0, "attempt-1", &stop).is_none());
        refresh().unwrap();
        assert_eq!(service.calls.load(Ordering::SeqCst), 2);
        let replacement = ledger.with_extension("new");
        std::fs::write(&replacement, "replacement").unwrap();
        std::fs::rename(replacement, &ledger).unwrap();
        service.fail.store(true, Ordering::SeqCst);
        assert!(refresh().is_err());
        service.fail.store(false, Ordering::SeqCst);
        refresh().unwrap();
        refresh().unwrap();
        assert_eq!(service.calls.load(Ordering::SeqCst), 4);
        // A later snapshot stage failure must discard an otherwise valid cache hit.
        assert!(
            oms_panel_core::state::snapshot_with_cache(
                &repo.0,
                Some("room"),
                &service,
                &stop,
                Some(&cache)
            )
            .is_err()
        );
        refresh().unwrap();
        assert_eq!(service.calls.load(Ordering::SeqCst), 5);
        std::fs::remove_file(&ledger).unwrap();
        std::os::unix::fs::symlink("missing", &ledger).unwrap();
        refresh().unwrap();
        refresh().unwrap();
        assert_eq!(service.calls.load(Ordering::SeqCst), 7);

        std::fs::remove_file(&ledger).unwrap();
        let target = json!({"attempt_id":"attempt-unique"});
        for (index, invalid) in [target.clone(), json!({"attempt_id":7})]
            .into_iter()
            .enumerate()
        {
            *service.rows.borrow_mut() = Some(json!([target, invalid]));
            std::fs::write(&ledger, "x".repeat(index + 1)).unwrap();
            refresh().unwrap();
            assert!(
                cache
                    .current_attempt(&repo.0, "attempt-unique", &stop)
                    .is_none()
            );
        }
        assert_eq!(service.calls.load(Ordering::SeqCst), 9);
    }

    #[test]
    fn finalized_outcomes_bind_to_the_displayed_plan_across_refreshes() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-panel-plan-bound-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let repo = TempRepo(dir);
        let repo = repo.0.as_path();
        let summary = "Reviewed fixture result";
        let result = serde_json::to_vec(&json!({"schema":1,"kind":"oms-panel-result",
            "task_id":"task-1","outcome":"completed","revision":"0a".repeat(12),
            "summary":summary,"summary_sha256":oms_panel_core::digest(summary.as_bytes()),
            "plan_binding":{"plan_id":"plan-1","task_sha256":"0b".repeat(32)}}))
        .unwrap();
        let artifact = ".oms/artifacts/panel-results/task-1.json";
        store_file(repo, artifact, &result);
        let row = json!({"kind":"panel-result","task_id":"task-1","artifact":artifact,
            "artifact_sha256":oms_panel_core::digest(&result)});
        store_file(
            repo,
            ".oms/artifacts/index.jsonl",
            format!("{row}\n").as_bytes(),
        );
        let unbound = json!({"plan_id":"plan-1","tasks":{}});
        let bound =
            json!({"plan_id":"plan-1","tasks":{"task-1":{"id":"task-1","state":"running"}}});
        let tasks = ".oms/plan/tasks.json";
        store_file(repo, tasks, &serde_json::to_vec(&unbound).unwrap());
        let session = Session::new();
        let refresh = || {
            snapshot_with(
                &session,
                repo,
                &Options::default(),
                &Attempts,
                &AtomicBool::new(false),
            )
            .unwrap()
            .report
        };
        let first = refresh();
        assert_eq!(first["plan"], unbound);
        assert_eq!(first["finalized"]["task-1"], "completed");
        // A changed plan whose stamp a cache still trusts (coarse timestamps)
        // must not leave `finalized` bound to the superseded plan.
        store_file(repo, tasks, &serde_json::to_vec(&bound).unwrap());
        let print = oms_panel_core::cache::stamp(&repo.join(tasks));
        session.cache.insert("plan".into(), print, unbound).unwrap();
        let second = refresh();
        assert_eq!(second["plan"], bound);
        assert!(second["finalized"].get("task-1").is_none());
    }

    #[test]
    fn leased_task_keeps_unstamped_finished_delegate_in_attention() {
        let report = json!({
            "plan":{"tasks":{"task-1":{"id":"task-1","state":"running","lease_id":"current"}}},
            "attempts":{"recent":[{"attempt_id":"worker","tool":"peer-delegate",
                "state":"done","terminal":true,"task_id":"task-1","refs":{}}]}
        });
        let items = crate::attention::items(&report, &Default::default());
        assert!(
            items
                .iter()
                .any(|item| item.what == "workers ended, still held"),
            "{items:?}"
        );
    }

    #[test]
    fn leased_task_keeps_live_unstamped_reviewers_and_excludes_previous_lease() {
        for tool in ["peer-review", "peer-ask"] {
            let mut report = json!({
                "plan":{"tasks":{"task-1":{"id":"task-1","state":"running","lease_id":"current"}}},
                "attempts":{"recent":[
                    {"attempt_id":"worker","tool":"peer-delegate","state":"done","terminal":true,
                        "task_id":"task-1","refs":{"panel_lease_id":"current"}},
                    {"attempt_id":"old","tool":"peer-delegate","state":"working","terminal":false,
                        "task_id":"task-1","refs":{"lease_id":"previous"}}
                ]}
            });
            let held = |report: &serde_json::Value| {
                crate::attention::items(report, &Default::default())
                    .iter()
                    .any(|item| item.what == "workers ended, still held")
            };
            assert!(held(&report));
            report["attempts"]["recent"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "attempt_id":"reviewer","tool":tool,"state":"working","terminal":false,
                    "task_id":"task-1","refs":{}
                }));
            assert!(!held(&report), "{tool}");
        }
    }

    #[test]
    fn plan_attention_uses_the_canonical_lease_verdict_and_keeps_legacy_held_rows() {
        use oms_command_plan::{Action, Context, Plan, Request, UnavailableServices};
        struct TimedPlan(Context);
        impl PeerExecutor for TimedPlan {
            fn execute(
                &self,
                request: &ExecutionRequest,
                stop: &AtomicBool,
            ) -> Result<CommandOutput> {
                Attempts.execute(request, stop)
            }
        }
        impl Services for TimedPlan {
            fn command(
                &self,
                command: NativeCommand,
                repo: &Path,
                args: &[OsString],
                stop: &AtomicBool,
            ) -> Result<CommandOutput> {
                if command != NativeCommand::Plan {
                    return Attempts.command(command, repo, args, stop);
                }
                assert_eq!(args, ["list", "--json"]);
                let plan = Plan::open(self.0.clone(), &UnavailableServices, stop).unwrap();
                let response = plan
                    .execute(&Request::new(Action::List).flag("json"))
                    .unwrap();
                Ok(CommandOutput {
                    code: response.code,
                    stdout: response.stdout,
                    stderr: response.stderr,
                    cleanup_complete: true,
                    output_complete: true,
                    ..Default::default()
                })
            }
        }
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("oms-panel-lease-attention-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let repo = TempRepo(dir);
        let repo = repo.0.as_path();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q", "--"])
                .arg(repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args([
                    "remote",
                    "add",
                    "origin",
                    "git@github.com:example-org/example-repo.git"
                ])
                .status()
                .unwrap()
                .success()
        );
        let stop = AtomicBool::new(false);
        let request = Request::new(Action::List).flag("json");
        let mut context = Context::from_selected_state_before(
            &request,
            repo,
            repo,
            repo,
            &stop,
            std::time::Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        context.ts = "2026-11-24T12:00:00Z".into();
        let service = TimedPlan(context);
        let reference = "example-org/example-repo#19";
        let captured = json!("2026-11-24T11:59:00Z");
        let cache = json!({
            "reference":reference,"kind":"pull","state":"merged","title":"Cached title",
            "labels":[],"body_sha256":"a".repeat(64),"ttl_s":900,
            "captured_at":oms_panel_core::metrics::stamp(&captured).unwrap() as u64
        });
        store_file(
            repo,
            ".oms/github/cache.jsonl",
            cache.to_string().as_bytes(),
        );
        let lease = format!("lease_{}", "1".repeat(32));
        for state in ["claimed", "running"] {
            let plan = json!({"plan_id":format!("plan_{}", "2".repeat(32)), "tasks":{
                "task-1":{"id":"task-1","title":"Leased task","state":state,"depends":[],
                    "claimed_at":"2026-11-24T10:00:00Z","ttl":"3600","lease_id":lease},
                "task-2":{"id":"task-2","title":"Legacy task","status":"held","depends":[]},
                "task-3":{"id":"task-3","title":"Review task","state":"review","depends":[],"issue":reference},
                "task-4":{"id":"task-4","title":"Active task","state":"running","depends":[],
                    "claimed_at":"2026-11-24T11:59:00Z","ttl":"3600","lease_id":lease},
            }});
            store_file(repo, ".oms/plan/tasks.json", plan.to_string().as_bytes());
            let marker_path = ".oms/delegations/writer.json";
            for pid in [None, Some(std::process::id()), Some(0)] {
                if let Some(pid) = pid {
                    let marker = json!({"schema":1,"id":"writer","task_id":"task-1","lease_id":lease,"pid":pid});
                    store_file(repo, marker_path, marker.to_string().as_bytes());
                }
                let mut report =
                    snapshot_with(&Session::new(), repo, &Options::default(), &service, &stop)
                        .unwrap()
                        .report;
                assert_eq!(report["plan"], plan);
                report["attempts"]["recent"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({
                        "attempt_id":"worker-2","tool":"peer-delegate","provider":"codex",
                        "state":"done","terminal":true,"task_id":"task-2","refs":{}
                    }));
                report["attempts"]["recent"].as_array_mut().unwrap().extend([
                    json!({"attempt_id":"worker-current","tool":"peer-delegate","provider":"codex",
                        "state":"done","terminal":true,"task_id":"task-4","refs":{"panel_lease_id":lease}}),
                    json!({"attempt_id":"worker-previous","tool":"peer-delegate","provider":"codex",
                        "state":"working","terminal":false,"task_id":"task-4",
                        "refs":{"panel_lease_id":format!("lease_{}", "3".repeat(32))}}),
                ]);
                let attention = crate::attention::items(&report, &Default::default());
                let summary = |id: &str| {
                    attention
                        .iter()
                        .find(|item| item.row_id == ("task".into(), id.into()))
                        .map(|item| (item.cls.as_str(), item.what.as_str()))
                };
                assert_eq!(
                    summary("task-1"),
                    Some((
                        "stale",
                        if pid.is_none() {
                            "Plan lease expired"
                        } else {
                            "workers ended, still held"
                        }
                    ))
                );
                assert_eq!(
                    summary("task-2"),
                    Some(("stale", "workers ended, still held"))
                );
                assert_eq!(
                    summary("task-4"),
                    Some(("stale", "workers ended, still held"))
                );
                assert_eq!(
                    summary("task-3"),
                    Some((
                        "decide",
                        "PR example-org/example-repo#19 merged (fresh) · completed-external candidate"
                    ))
                );
                let (_, work, _) = crate::desktop_board::work_items(repo, &service, &stop).unwrap();
                let work: serde_json::Value = serde_json::from_slice(&work).unwrap();
                let task = work["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|task| task["id"] == "task-1")
                    .unwrap();
                assert_eq!(task["state"], if pid.is_none() { "stale" } else { state });
                assert_eq!(task["claimExpired"], pid.is_none());
                let review = work["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|task| task["id"] == "task-3")
                    .unwrap();
                assert_eq!(review["issueCache"]["reason"], "no matching cache entry");
                assert_eq!(
                    review["prReviewCandidate"]["disposition"],
                    "completed-external"
                );
                assert!(!work.to_string().contains("Cached title"));
            }
            std::fs::remove_file(repo.join(marker_path)).unwrap();
        }
    }

    fn main_report(room: &str, attempt: &str, consumer: &str) -> serde_json::Value {
        json!({"room":{"id":room,"owner":attempt,"closed":false,"participants":[
            {"participant":attempt,"role":"main","provider":"codex","joined":true,"consumer":consumer}]},
            "attempts":{"active":[{"attempt_id":attempt,"tool":"panel-main","provider":"codex",
            "terminal":false,"state":"working","refs":{"panel_room_id":room}}]}})
    }
    fn turn(model: &str) -> String {
        format!("{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"{model}\"}}}}\n")
    }
    fn rollout_fixture(dir: &Path) -> (PathBuf, String) {
        let id = "019e0000-0000-7000-8000-000000000001";
        let consumer = oms_panel_core::digest(id.as_bytes())[..32].to_owned();
        let path = dir.join("rollout.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\"}}}}\n{}",
                turn("gpt-6.1")
            ),
        )
        .unwrap();
        (path, consumer)
    }
    fn append(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    #[test]
    fn provider_status_key_ignores_worker_churn_but_not_main_changes() {
        let mut report = main_report("room-1", "main-1", "c1");
        let base = super::main_candidates_key(&report);
        let rows = report["room"]["participants"].as_array_mut().unwrap();
        rows.push(json!({"participant":"w1","role":"worker","provider":"codex","state":"working"}));
        assert_eq!(super::main_candidates_key(&report), base);
        report["room"]["participants"][0]["model"] = json!(null);
        let model_key = super::main_candidates_key(&report);
        assert_ne!(model_key, base);
        report["room"]["participants"][0]["state"] = json!("done");
        assert_ne!(super::main_candidates_key(&report), model_key);
    }

    #[test]
    fn main_model_observation_retains_until_identity_changes() {
        let dir = TempRepo(std::env::temp_dir().join(format!("oms-r43c-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let (path, consumer) = rollout_fixture(&dir.0);
        let session = Session::new();
        let locate = |c: &str| (c == consumer).then(|| path.clone());
        let observe =
            |report: &serde_json::Value| session.observe_main_model_with(&dir.0, report, locate);
        let report = main_report("room-1", "main-1", &consumer);
        let fresh = observe(&report).unwrap();
        assert_eq!(
            fresh,
            json!({"attempt":"main-1","model":"gpt-6.1","source":"rollout"})
        );

        // A large tail with no turn_context pushes the last one out of the read window.
        let big = format!(
            "{{\"type\":\"event_msg\",\"x\":\"{}\"}}\n",
            "a".repeat(2 * 1024 * 1024)
        );
        append(&path, big.as_bytes());
        let kept = observe(&report).unwrap();
        assert_eq!(kept["model"], "gpt-6.1");
        assert_eq!(kept["source"], "rollout-last");
        assert_eq!(observe(&report).unwrap()["source"], "rollout-last");

        // A partial last line does not disturb it either.
        append(&path, b"{\"type\":\"turn_con");
        assert_eq!(observe(&report).unwrap()["source"], "rollout-last");

        // A newer turn_context replaces it and is fresh again.
        append(&path, b"\n");
        append(&path, turn("gpt-6.2-sol").as_bytes());
        let newer = observe(&report).unwrap();
        assert_eq!(newer["model"], "gpt-6.2-sol");
        assert_eq!(newer["source"], "rollout");

        // Retained value survives only for the same room, attempt and consumer.
        append(&path, big.as_bytes());
        assert_eq!(observe(&report).unwrap()["source"], "rollout-last");
        assert!(observe(&main_report("room-2", "main-1", &consumer)).is_none());
        // The mismatch dropped it: the next read has no turn_context to show.
        assert!(observe(&report).is_none());
    }

    #[test]
    fn main_rollout_tokens_follow_the_exact_session_and_survive_a_bounded_tail() {
        use serde_json::Value;

        let dir =
            TempRepo(std::env::temp_dir().join(format!("oms-main-tokens-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let (path, consumer) = rollout_fixture(&dir.0);
        let id = "019e0000-0000-7000-8000-000000000001";
        let session = Session::new();
        let report = main_report("room-1", "main-1", &consumer);
        let observe = |report: &Value| {
            session
                .observe_main_with(&dir.0, report, |c| (c == consumer).then(|| path.clone()))
                .and_then(|entry| entry.tokens)
        };
        let usage = |thread: &str, root: &str, total: Value| {
            json!({"type":"token_usage_record","payload":{
                "thread_id":thread,"session_id":root,"thread_token_usage":{"total_tokens":total}
            }})
        };
        let write_usage = |record: Value| append(&path, format!("{record}\n").as_bytes());
        assert!(observe(&report).is_none());
        write_usage(usage(id, id, json!(0)));
        assert_eq!(observe(&report).unwrap()["total"], 0);
        write_usage(usage(id, id, json!(1_025_123_456u64)));
        for record in [
            usage("other", id, json!(900)),
            usage(id, "other", json!(901)),
            usage(id, id, json!(-1)),
            usage(id, id, json!(120.5)),
            json!({"type":"event_msg","payload":usage(id, id, json!(92))}),
        ] {
            write_usage(record);
        }
        append(&path, usage(id, id, json!(99)).to_string().as_bytes());
        assert_eq!(
            observe(&report).unwrap(),
            json!({"room":"room-1","participant":"main-1","attempt":"main-1",
                "total":1_025_123_456u64,"source":"rollout"})
        );
        append(
            &path,
            format!(
                "\n{{\"type\":\"event_msg\",\"x\":\"{}\"}}\n",
                "a".repeat(1024 * 1024)
            )
            .as_bytes(),
        );
        let retained = observe(&report).unwrap();
        assert_eq!(retained["source"], "rollout-last");
        assert_eq!(retained["total"], 1_025_123_456u64);
        assert!(observe(&main_report("room-2", "main-1", &consumer)).is_none());
        assert!(observe(&report).is_none());
    }

    #[test]
    fn main_model_observation_drops_on_mismatch() {
        let dir =
            TempRepo(std::env::temp_dir().join(format!("oms-r43c-drop-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let (path, consumer) = rollout_fixture(&dir.0);
        let big = format!("{{\"x\":\"{}\"}}\n", "a".repeat(2 * 1024 * 1024));
        let locate = |c: &str| (c == consumer).then(|| path.clone());
        let report = main_report("room-1", "main-1", &consumer);
        let primed = || {
            rollout_fixture(&dir.0);
            let session = Session::new();
            assert_eq!(
                session
                    .observe_main_model_with(&dir.0, &report, locate)
                    .unwrap()["source"],
                "rollout"
            );
            append(&path, big.as_bytes());
            session
        };
        // A different attempt, room or consumer finds nothing fresh to show.
        let session = primed();
        assert!(
            session
                .observe_main_model_with(
                    &dir.0,
                    &main_report("room-1", "main-2", &consumer),
                    locate
                )
                .is_none()
        );
        let session = primed();
        assert!(
            session
                .observe_main_model_with(
                    &dir.0,
                    &main_report("room-9", "main-1", &consumer),
                    locate
                )
                .is_none()
        );
        let session = primed();
        let other = "0".repeat(32);
        assert!(
            session
                .observe_main_model_with(&dir.0, &main_report("room-1", "main-1", &other), locate)
                .is_none()
        );

        // Two active Codex Mains are ambiguous.
        let session = primed();
        let mut two = report.clone();
        two["attempts"]["active"]
            .as_array_mut()
            .unwrap()
            .push(json!({
            "attempt_id":"main-2","tool":"panel-main","provider":"codex",
            "terminal":false,"refs":{"panel_room_id":"room-1"}}));
        assert!(
            session
                .observe_main_model_with(&dir.0, &two, locate)
                .is_none()
        );
        // ... and the retained value did not outlive the ambiguity.
        assert!(
            session
                .observe_main_model_with(&dir.0, &report, locate)
                .is_none()
        );

        // In-place truncate and rewrite with another session_meta, shorter than before.
        let session = primed();
        std::fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n",
        )
        .unwrap();
        assert!(
            session
                .observe_main_model_with(&dir.0, &report, locate)
                .is_none()
        );

        // A second hard link makes the file unfit to read or retain.
        let session = primed();
        std::fs::hard_link(&path, dir.0.join("alias.jsonl")).unwrap();
        assert!(
            session
                .observe_main_model_with(&dir.0, &report, locate)
                .is_none()
        );
        std::fs::remove_file(dir.0.join("alias.jsonl")).unwrap();

        // A replaced rollout file (new inode, same path, no turn_context) drops it.
        let session = primed();
        let first = std::fs::read(&path).unwrap();
        let first = &first[..=first.iter().position(|&b| b == b'\n').unwrap()];
        let staged = dir.0.join("staged.jsonl");
        std::fs::write(&staged, first).unwrap();
        std::fs::rename(&staged, &path).unwrap();
        assert!(
            session
                .observe_main_model_with(&dir.0, &report, locate)
                .is_none()
        );
    }

    #[test]
    fn sessions_preserves_waiting_and_unknown_parity_across_custodian_and_age_cases() {
        use super::{Value, rows, strings, text};
        let now = oms_panel_core::timestamp();
        let main_attempt = json!({
            "attempt_id": "att-main",
            "tool": "panel-main",
            "provider": "codex",
            "state": "working",
            "terminal": false,
            "created_at": now - 60,
            "refs": {
                "panel_room_id": "room-demo",
                "panel_role": "main",
                "panel_room_participant": "main-1"
            }
        });
        let report = json!({
            "generated_at": now,
            "collection": {"ok": true},
            "capacity_stale": false,
            "room": {
                "id": "room-demo",
                "owner": "main-1",
                "participants": [
                    {
                        "participant": "main-1",
                        "role": "main",
                        "provider": "codex",
                        "joined": true,
                        "joined_at": now - 60
                    },
                    {
                        "participant": "live-res",
                        "role": "worker",
                        "provider": "codex",
                        "joined": true
                    },
                    {
                        "participant": "live-queued",
                        "role": "worker",
                        "provider": "claude",
                        "state": "queued",
                        "joined": true,
                        "joined_at": now - 30
                    },
                    {
                        "participant": "live-wait",
                        "role": "worker",
                        "provider": "antigravity",
                        "state": "waiting_input",
                        "joined": true,
                        "joined_at": now - 30
                    },
                    {
                        "participant": "undated-queued",
                        "role": "worker",
                        "provider": "codex",
                        "state": "queued",
                        "joined": true
                    },
                    {
                        "participant": "left-queued",
                        "role": "worker",
                        "provider": "claude",
                        "state": "queued",
                        "joined": false,
                        "joined_at": now - 30
                    },
                    {
                        "participant": "undated-unk",
                        "role": "worker",
                        "provider": "antigravity",
                        "state": "unknown",
                        "joined": true
                    },
                    {
                        "participant": "ancient-queued",
                        "role": "worker",
                        "provider": "codex",
                        "state": "queued",
                        "joined": true,
                        "joined_at": now - 600
                    },
                    {
                        "participant": "stale-cust-disp",
                        "role": "worker",
                        "provider": "claude",
                        "state": "queued",
                        "joined": true,
                        "joined_at": now - 30
                    },
                    {
                        "participant": "ancient-old-cust",
                        "role": "worker",
                        "provider": "codex",
                        "state": "queued",
                        "joined": true,
                        "joined_at": now - 600
                    }
                ]
            },
            "room_attempts": {
                "att-main": main_attempt,
                "main-1": main_attempt,
                "att-old-main": {
                    "attempt_id": "att-old-main",
                    "tool": "panel-main",
                    "provider": "codex",
                    "state": "done",
                    "terminal": true,
                    "created_at": now - 900,
                    "updated_at": now - 600,
                    "refs": {
                        "panel_room_id": "room-demo",
                        "panel_role": "main",
                        "panel_room_participant": "main-1"
                    }
                }
            },
            "room_dispatches": {
                "live-res": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-main",
                    "state": "reserved",
                    "created_at": now - 30
                },
                "stale-cust-disp": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-old-main",
                    "state": "running",
                    "created_at": now - 30
                },
                "ancient-old-cust": {
                    "room": "room-demo",
                    "main": "main-1",
                    "main_attempt": "att-old-main",
                    "state": "running",
                    "created_at": now - 600
                }
            }
        });

        let query_ids = |rep: &Value, state_filter: &str| -> std::collections::BTreeSet<String> {
            let opts = Options::parse(&strings(&["--sessions", "--json", "--state", state_filter]))
                .unwrap();
            let out = super::sessions(rep, &opts).unwrap();
            let payload: Value = serde_json::from_slice(&out.1).unwrap();
            rows(&payload["sessions"])
                .iter()
                .map(|row| text(row, "participant").to_owned())
                .collect()
        };
        let visible_ids = |rep: &Value| -> std::collections::BTreeSet<String> {
            crate::render::visible(rep, &crate::navigation::Navigation::default())
                .into_iter()
                .map(|row| text(&row, "participant").to_owned())
                .collect()
        };

        let auto = visible_ids(&report);
        assert!(auto.contains("live-res"), "{auto:?}");
        assert!(auto.contains("live-queued"), "{auto:?}");
        assert!(auto.contains("live-wait"), "{auto:?}");
        for folded in [
            "undated-queued",
            "left-queued",
            "undated-unk",
            "ancient-queued",
            "stale-cust-disp",
            "ancient-old-cust",
        ] {
            assert!(!auto.contains(folded), "{folded} leaked: {auto:?}");
        }

        let waiting = query_ids(&report, "waiting");
        for expected in [
            "live-queued",
            "live-wait",
            "undated-queued",
            "left-queued",
            "stale-cust-disp",
        ] {
            assert!(waiting.contains(expected), "{expected}: {waiting:?}");
        }
        assert!(!waiting.contains("ancient-queued"), "{waiting:?}");
        assert!(!waiting.contains("ancient-old-cust"), "{waiting:?}");
        assert!(!waiting.contains("undated-unk"), "{waiting:?}");

        let unknown = query_ids(&report, "unknown");
        assert!(unknown.contains("ancient-queued"), "{unknown:?}");
        assert!(unknown.contains("ancient-old-cust"), "{unknown:?}");
        assert!(unknown.contains("undated-unk"), "{unknown:?}");

        let mut no_main = report.clone();
        no_main["room_attempts"]
            .as_object_mut()
            .unwrap()
            .remove("att-main");
        no_main["room_attempts"]
            .as_object_mut()
            .unwrap()
            .remove("main-1");
        let mut closed = report.clone();
        closed["room"]["closed"] = json!(true);

        for (label, rep) in [("no_main", &no_main), ("closed", &closed)] {
            let rep_auto = visible_ids(rep);
            assert!(!rep_auto.contains("live-res"), "{label}: {rep_auto:?}");
            assert!(!rep_auto.contains("live-queued"), "{label}: {rep_auto:?}");
            assert!(
                !rep_auto.contains("ancient-queued"),
                "{label}: {rep_auto:?}"
            );

            let rep_waiting = query_ids(rep, "waiting");
            for expected in ["live-queued", "undated-queued", "left-queued"] {
                assert!(
                    rep_waiting.contains(expected),
                    "{label} {expected}: {rep_waiting:?}"
                );
            }
            assert!(
                !rep_waiting.contains("ancient-queued"),
                "{label}: {rep_waiting:?}"
            );

            let rep_unknown = query_ids(rep, "unknown");
            assert!(
                rep_unknown.contains("ancient-queued"),
                "{label}: {rep_unknown:?}"
            );
            assert!(
                rep_unknown.contains("undated-unk"),
                "{label}: {rep_unknown:?}"
            );
        }

        let mut closed_question = closed.clone();
        closed_question["room"]["participants"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "participant": "closed-question-queued",
                "role": "worker",
                "provider": "codex",
                "state": "queued",
                "joined": true,
                "joined_at": now - 600
            }));
        closed_question["room"]["open_questions"] = json!([
            {
                "sender": "closed-question-queued",
                "recipient": "main-1",
                "text": "Need decision before exit"
            }
        ]);
        let closed_q_auto = visible_ids(&closed_question);
        assert!(
            !closed_q_auto.contains("closed-question-queued"),
            "{closed_q_auto:?}"
        );
        let closed_q_call = crate::render::calls(&closed_question)
            .into_iter()
            .find(|row| row["participant"] == "closed-question-queued")
            .unwrap();
        assert_eq!(closed_q_call["room_only_stale"], true, "{closed_q_call}");
        assert!(
            query_ids(&closed_question, "waiting").contains("closed-question-queued"),
            "closed-question-queued must remain waiting in --sessions"
        );
        assert!(
            !query_ids(&closed_question, "unknown").contains("closed-question-queued"),
            "closed-question-queued must not turn unknown in --sessions"
        );
    }
}
