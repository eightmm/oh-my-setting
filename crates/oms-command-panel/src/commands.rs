use crate::{
    Error, Output, Result, Services, cli::Options, json, parent_only, require, rows, text,
};
use oms_panel_core::{
    NativeCommand, Store, Value, dispatch, metrics, release, results, review, routing, salvage,
    state, terminals,
};
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
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
                .is_none_or(|main| v["refs"]["panel_room_participant"] == *main)
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
    metrics: std::sync::Mutex<Option<(std::time::Instant, String, Value)>>,
}
const METRICS_TTL: Duration = Duration::from_secs(15);
impl Session {
    pub(crate) fn new() -> Self {
        Self {
            cache: oms_panel_core::cache::Cache::new(256, Duration::from_secs(3600))
                .expect("static cache limits"),
            metrics: Default::default(),
        }
    }
    fn plan(&self, repo: &Path, store: &Store) -> Result<Value> {
        let path = Path::new(".oms/plan/tasks.json");
        let print = oms_panel_core::cache::stamp(&repo.join(path));
        if let Some(v) = self.cache.read("plan", &print) {
            return Ok(v);
        }
        let v = store
            .json(path, 4 * 1024 * 1024, true)?
            .unwrap_or(Value::Null);
        let _ = self.cache.insert("plan".into(), print, v.clone());
        Ok(v)
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
    fn provider_status(
        &self,
        repo: &Path,
        report: &Value,
        main: Option<&str>,
        rows: &[Value],
    ) -> Value {
        let print = oms_panel_core::digest(
            format!(
                "{}|{:?}|{}|{}",
                report["room"]["participants"],
                main,
                oms_panel_core::cache::stamp(&repo.join(".oms/artifacts/index.jsonl")),
                sessions_stamp()
            )
            .as_bytes(),
        );
        if let Ok(guard) = self.metrics.lock()
            && let Some((at, key, value)) = guard.as_ref()
            && *key == print
            && at.elapsed() < METRICS_TTL
        {
            return value.clone();
        }
        let value = metrics::collect_local_with(report, main, unix_now(), Some(rows));
        if let Ok(mut guard) = self.metrics.lock() {
            *guard = Some((std::time::Instant::now(), print, value.clone()));
        }
        value
    }
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
pub(crate) fn snapshot(
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<state::Snapshot> {
    snapshot_with(&Session::new(), repo, options, services, stop)
}
pub(crate) fn snapshot_with(
    session: &Session,
    repo: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<state::Snapshot> {
    let mut snapshot = state::snapshot(repo, room(options).as_deref(), services, stop)?;
    let tasks: Vec<_> = crate::render::calls(&snapshot.report)
        .iter()
        .filter_map(|v| {
            v["task_id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .collect();
    let store = Store::open(repo)?;
    let plan = session.plan(repo, &store)?;
    let index = session.index(repo)?;
    let decisions =
        results::outcomes_cached(&store, &plan, &index, &tasks, Some((&session.cache, repo)))?;
    let mut finalized = serde_json::Map::new();
    for decision in decisions {
        if let Some(task) = decision["task_id"].as_str() {
            finalized.insert(task.into(), decision["outcome"].clone());
        }
    }
    snapshot.report["finalized"] = Value::Object(finalized);
    let main_attempt = options
        .get("owner-attempt")
        .map(str::to_owned)
        .or_else(|| snapshot.report["room"]["main"].as_str().map(str::to_owned));
    snapshot.report["provider_status"] =
        session.provider_status(repo, &snapshot.report, main_attempt.as_deref(), &index);
    Ok(snapshot)
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
    let explicit_raw = options.get("repo").map(|raw| {
        let p = PathBuf::from(raw);
        if p.is_absolute() { p } else { cwd.join(p) }
    });
    let optional_git = options.command.is_none()
        && ((options.has("providers") || options.has("routes")) && explicit_raw.is_none()
            || (options.has("recommend") && !options.has("diff") && explicit_raw.is_none()));
    let requires_room_match = options.command.is_some()
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
    let env_room = std::env::var_os("OMS_ROOM_REPO").map(PathBuf::from);
    let (repo_opt, exec_opt) = resolve_repositories_with(options, cwd, env_room.as_deref())?;
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
        return routes_output(repo.as_deref(), cwd, source, options, services, stop);
    }
    let repo = repo_req()?;
    let execution = execution_req()?;
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
        return output(release::swap_main(
            &repo,
            &release::MainSwapRequest {
                attempt_id: main_attempt(&repo, options, services, stop)?,
                provider,
                model,
                harness,
            },
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
        return plain(format!(
            "committed locally; publication pending: {}\nNext: oms land --repo {}\nThen: oms panel --finalize --repo {} --landed-commit <full head SHA that oms land pushed>\n",
            text(&accepted, "sha"),
            shell_quote(&execution.to_string_lossy()),
            shell_quote(&execution.to_string_lossy())
        ));
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
    if options.has("chats") || options.has("open-chat") {
        return chats(&repo, options, services, stop);
    }
    if options.has("watch") || options.has("log") {
        return crate::watch::run(&repo, options, services, stop);
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
            artifacts: rows(&artifacts),
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
        let snap = crate::usage::snapshot(&state.report, unix_now());
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
    crate::watch::run(&repo_req()?, options, services, stop)
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
fn routes_output(
    repo: Option<&Path>,
    cwd: &Path,
    source: &Path,
    options: &Options,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<Output> {
    let policy = policy(source)?;
    let owner = owner(options);
    let inputs = routing_inputs(
        repo,
        Some(cwd),
        room(options).as_deref(),
        Some(&owner),
        services,
        stop,
    )?;
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
        let mut role_inputs = inputs.clone();
        if let Some(target) = route_target(options)?.0 {
            role_inputs.preferred = Some(target);
            role_inputs.explicit_target = true;
        }
        match routing::allocate(&policy, &owner, role, None, access, None, &role_inputs) {
            Ok(alloc) => {
                chosen.insert(
                    role.clone(),
                    json!({
                        "provider": alloc.candidate.provider,
                        "model": alloc.candidate.model,
                        "effort": alloc.candidate.effort,
                        "snapshot": alloc.snapshot,
                    }),
                );
            }
            Err(err) => {
                chosen.insert(role.clone(), json!({"error": err.message}));
            }
        }
    }
    if let Some(obj) = base.as_object_mut() {
        obj.insert("writers".into(), json!(writers));
        obj.insert("kinds".into(), json!(kinds));
        obj.insert("owner".into(), json!(owner));
        obj.insert("chosen".into(), Value::Object(chosen));
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

fn routing_inputs(
    repo: Option<&Path>,
    cwd: Option<&Path>,
    room_id: Option<&str>,
    owner_hint: Option<&str>,
    services: &dyn Services,
    stop: &AtomicBool,
) -> Result<routing::RoutingInputs> {
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
        let mut models = strings("models");
        if models.is_empty() {
            models = strings("catalog_models");
        }
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

    let mut report_stub = Value::Null;
    let mut ambiguous_active_main = false;
    if let Some(repo_path) = repo {
        let attempts = query(
            services,
            NativeCommand::Events,
            repo_path,
            &["list", "--json"],
            stop,
        )
        .unwrap_or(Value::Null);
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
        for row in rows(&attempts) {
            let room_match = room_id.is_none_or(|id| {
                row["refs"]["panel_room_id"].as_str() == Some(id)
                    || row["refs"]["panel_room"].as_str() == Some(id)
                    || row["panel"]["room_id"].as_str() == Some(id)
            });
            let prov = text(row, "provider");
            if room_match
                && !prov.is_empty()
                && row["refs"]["panel_role"]
                    .as_str()
                    .is_some_and(|r| r != "main")
            {
                *inputs.recent.entry(prov.to_owned()).or_insert(0) += 1;
            }
            let state = text(row, "state");
            let is_main =
                text(&row["refs"], "panel_role") == "main" || text(row, "tool") == "panel-main";
            let is_terminal = row["terminal"] == true
                || (!state.is_empty() && oms_panel_core::state::TERMINAL_STATES.contains(&state));
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
            if state == "working" || state == "running" {
                let acc = text(&row["refs"], "panel_account");
                if !acc.is_empty() {
                    let key = if acc.contains(':') || prov.is_empty() {
                        acc.to_owned()
                    } else {
                        format!("{prov}:{acc}")
                    };
                    *inputs.running_accounts.entry(key).or_insert(0) += 1;
                }
            }
            if (prov == "pi" || text(&row["refs"], "panel_harness") == "pi")
                && (row["refs"]["daily_cap"] == true || text(row, "error").contains("daily"))
                && let Some(installed) = inputs.installed.as_mut()
            {
                installed.remove("pi");
            }
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

    inputs.status = metrics::collect_local(repo, &report_stub, None, unix_now());
    Ok(inputs)
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
        purpose = purpose.or(defaults.purpose);
        workload = workload.or(defaults.workload);
        verifier = verifier.or(defaults.verify);
        if !options.has("scope") {
            scopes = defaults.scopes;
        }
        continued_account = defaults.account;
        continued_provider = defaults.provider;
    }
    let mut inputs = routing_inputs(
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
