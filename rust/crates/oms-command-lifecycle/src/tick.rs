use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::AtomicBool};
fn registry(paths: &Paths) -> Result<PathBuf> {
    r::leaf(
        &std::env::var_os("OMS_TICK_REGISTRY")
            .map(PathBuf::from)
            .unwrap_or(paths.config.join("tick-repos.txt")),
    )
}
fn registered(paths: &Paths) -> Result<Vec<PathBuf>> {
    let file = registry(paths)?;
    if !r::exists(&file) {
        return Ok(Vec::new());
    }
    let raw = String::from_utf8(r::read(&file, r::LIMIT)?)
        .map_err(|_| r::Error(2, "tick registry is not UTF-8".into()))?;
    raw.lines()
        .filter(|s| !s.is_empty())
        .map(|s| r::physical(&PathBuf::from(s)))
        .collect()
}
pub(crate) fn run(
    paths: &Paths,
    options: &Options,
    flag: &AtomicBool,
    services: &dyn crate::Services,
) -> Result<crate::Output> {
    let action = if options.action.is_empty() {
        "run"
    } else {
        options.action.as_str()
    };
    if matches!(action, "install" | "uninstall") {
        return crate::scheduler::run(paths, options, flag);
    }
    if action == "schedule" {
        if !options.preview() && !options.has("--list") {
            r::child_guard()?;
        }
        return Ok(services.tick_schedule(&options.raw, &paths.cwd, &paths.source, flag));
    }
    if action == "refresh-catalogs" {
        if !options.preview() {
            r::child_guard()?;
        }
        return Ok(services.refresh_catalogs(options.preview(), &paths.source, flag));
    }
    let mut repos = options
        .all_paths("--repo")
        .iter()
        .map(|s| {
            r::physical(&if PathBuf::from(s).is_absolute() {
                PathBuf::from(s)
            } else {
                paths.cwd.join(s)
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if action == "register" || action == "unregister" {
        r::child_guard()?;
        if repos.is_empty() {
            repos.push(paths.cwd.clone());
        }
        let repos = repos
            .iter()
            .map(|p| {
                r::git(p, &["rev-parse", "--show-toplevel"], flag)
                    .map(PathBuf::from)
                    .and_then(|root| r::physical(&root))
                    .or_else(|_| r::physical(p))
            })
            .collect::<Result<Vec<_>>>()?;
        if action == "register"
            && repos.iter().any(|p| {
                !std::fs::symlink_metadata(p.join(".oms"))
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            })
        {
            return r::fail(
                "tick registration requires an adopted repository with a physical .oms directory",
            );
        }
        if repos
            .iter()
            .any(|p| p.to_str().is_none_or(|s| s.contains(['\n', '\r'])))
        {
            return r::fail("tick repository path contains a registry separator");
        }
        if options.preview() {
            return Ok(r::text(format!(
                "would {action} {} repositories",
                repos.len()
            )));
        }
        let file = registry(paths)?;
        let _guard = r::lock(&file, &paths.cwd)?;
        let mut existing = registered(paths)?;
        if action == "register" {
            for p in repos {
                if !existing.contains(&p) {
                    existing.push(p);
                }
            }
        } else {
            existing.retain(|p| !repos.contains(p));
        }
        existing.sort();
        existing.dedup();
        let body = existing
            .iter()
            .map(|p| format!("{}\n", p.to_str().unwrap()))
            .collect::<String>();
        r::atomic(&file, body.as_bytes())?;
        return Ok(r::text(format!(
            "tick registry: {} repositories",
            existing.len()
        )));
    }
    if repos.is_empty() {
        repos = registered(paths)?;
    }
    if action == "status" {
        let rows = repos.iter().map(|p|json!({"repo":p,"last":r::value(&p.join(".oms/tick/last.json")).unwrap_or(Value::Null)})).collect::<Vec<_>>();
        return r::json(&json!({"schema":1,"repositories":rows}));
    }
    if action != "run" {
        return r::fail("unknown tick action");
    }
    if options.preview() {
        return Ok(r::text(
            repos
                .iter()
                .map(|p| format!("would sweep {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    r::child_guard()?;
    for key in [
        "OMS_TICK_THREAD_IDLE_DAYS",
        "OMS_TICK_TASK_IDLE_DAYS",
        "OMS_TICK_PLAN_IDLE_DAYS",
        "OMS_TICK_FAILURE_STALE_DAYS",
        "OMS_TICK_STEP_TIMEOUT",
    ] {
        if std::env::var(key).ok().is_some_and(|s| {
            s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) || s.parse::<u64>().is_err()
        }) {
            return r::fail(format!("{key} must be an unsigned integer"));
        }
    }
    repos.sort();
    repos.dedup();
    let catalog = services.refresh_catalogs(false, &paths.source, flag);
    let mut receipts = Vec::new();
    for repo in repos {
        r::check(flag)?;
        if !std::fs::symlink_metadata(repo.join(".oms"))
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            receipts
                .push(json!({"repo":repo,"status":"skipped","reason":"repository is not adopted"}));
            continue;
        }
        let target = repo.join(".oms/tick/last.json");
        let guard = match r::lock(&target, &paths.cwd) {
            Ok(g) => g,
            Err(_) => {
                receipts.push(
                    json!({"repo":repo,"status":"deferred","reason":"tick lock unavailable"}),
                );
                continue;
            }
        };
        if maintenance_active(&target, &paths.cwd).unwrap_or(true) {
            receipts.push(json!({"repo":repo,"status":"deferred","reason":"verification running or identity unproven"}));
            continue;
        }
        let kept = kept_receipt(&target)?;
        let args = [
            "--repo".into(),
            repo.as_os_str().to_owned(),
            "reconcile".into(),
            "--apply".into(),
        ];
        let reconcile =
            oms_command_operations::run(oms_command_operations::App::Events, &args, &repo, flag);
        let memory = if repo.join(".oms/memory").is_dir() {
            Some(
                oms_command_memory::run_with_source(
                    &[
                        "--repo".into(),
                        repo.as_os_str().to_owned(),
                        "health".into(),
                        "--json".into(),
                    ],
                    &repo,
                    &paths.source,
                    flag,
                )
                .0,
            )
        } else {
            None
        };
        let telemetry = oms_command_telemetry::run(
            &[
                "telemetry".into(),
                "--repo".into(),
                repo.as_os_str().to_owned(),
                "--json".into(),
            ],
            &repo,
            flag,
        );
        let mut row = json!({"schema":1,"ran_at":r::now(),"repo":repo,"status":"failed","journal_rc":null,"reconcile_rc":reconcile.0,"memory_rc":memory,"telemetry_rc":telemetry.0,"gc":"skipped","threads_closed":[],"tasks_closed":0,"plans_retired":0,"artifacts_resolved":0,"artifacts_superseded":0,"failures_retired":0,"wake":"skipped","unavailable":[]});
        let output = services.tick_maintenance(&repo, &paths.source, flag);
        let maintenance = if output.0 == 0 {
            r::parse(&output.1).ok().filter(valid_maintenance)
        } else {
            None
        };
        if let Some(maintenance) = maintenance {
            for (key, value) in maintenance.as_object().unwrap() {
                row[key] = value.clone();
            }
            row["status"] = json!(if reconcile.0 == 0
                && telemetry.0 == 0
                && memory.is_none_or(|n| n == 0)
            {
                "ok"
            } else {
                "failed"
            });
        } else {
            row["unavailable"] =
                json!(["native journal/CI/retirement/artifact/failure maintenance receipt"]);
            let classified = oms_command_failure::run(
                &[
                    "classify".into(),
                    "--exit".into(),
                    output.0.max(1).to_string().into(),
                    "native tick maintenance failed".into(),
                ],
                &repo,
                flag,
            );
            if classified.0 == 0 {
                row["failure"] = r::parse(&classified.1).unwrap_or(Value::Null);
            }
        }
        row["catalog_rc"] = json!(catalog.0);
        if catalog.0 != 0 {
            row["status"] = json!("failed");
        }
        if std::env::var_os("OMS_TICK_GC").is_some_and(|v| v == "1") {
            let gc = services.garbage_collect(&repo, flag);
            row["gc"] = json!(gc.0);
            if gc.0 != 0 {
                row["status"] = json!("failed");
            }
        }
        if let Some(wakes) = kept.get("wakes") {
            row["wakes"] = wakes.clone();
        }
        let wake = std::env::var_os("OMS_TICK_WAKE").is_some_and(|v| v == "1");
        if wake || r::exists(&repo.join(".oms/tick/schedules.jsonl")) {
            let output = services.wake_idle_main(!wake, &repo, flag);
            if output.0 == 0 {
                let receipt = r::parse(&output.1)?;
                validate_wakes(&receipt)?;
                row["wake"] = receipt.get("wake").cloned().unwrap_or(json!(0));
                let day = &row["ran_at"].as_str().unwrap()[..10];
                if let Some(old) = kept["wakes"].as_array() {
                    let new = receipt["wakes"].as_array();
                    if old
                        .iter()
                        .filter(|w| w["day"] == day)
                        .any(|w| !new.is_some_and(|a| a.contains(w)))
                    {
                        return r::fail(
                            "wake callback dropped a current daily ledger entry; sweep publication refused",
                        );
                    }
                }
                if receipt["status"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("refused:"))
                    || !receipt["wakes"].is_array()
                {
                    return r::fail(
                        "wake callback omitted or refused its daily ledger; sweep publication refused",
                    );
                }
                if let Some(wakes) = receipt.get("wakes") {
                    row["wakes"] = wakes.clone();
                }
            } else {
                let refreshed = kept_receipt(&target)?;
                if let Some(wakes) = refreshed.get("wakes") {
                    row["wakes"] = wakes.clone();
                } else {
                    row.as_object_mut().unwrap().remove("wakes");
                }
                row["status"] = json!("failed");
                row["wake"] = json!(output.0);
            }
        }
        r::check(flag)?;
        let body = r::bytes(&row)?;
        if body.len() > 1024 * 1024 {
            return r::fail("tick receipt exceeds 1 MiB; previous receipt retained");
        }
        r::atomic(&target, &body)?;
        receipts.push(row);
        drop(guard);
    }
    let failed = catalog.0 != 0
        || receipts
            .iter()
            .any(|r| matches!(r["status"].as_str(), Some("failed" | "deferred")));
    let mut out = r::json(&json!({"schema":1,"sweeps":receipts}))?;
    out.0 = i32::from(failed);
    Ok(out)
}
fn validate_wakes(row: &Value) -> Result<()> {
    if !row.is_object()
        || row.get("wakes").is_some_and(|v| {
            v.as_array().is_none_or(|a| {
                a.iter().any(|r| {
                    !r["day"].as_str().is_some_and(|s| !s.is_empty())
                        || !r["main"].as_str().is_some_and(|s| !s.is_empty())
                        || !r["key"].as_str().is_some_and(|s| !s.is_empty())
                        || !matches!(r["outcome"].as_str(), Some("woke" | "resumed" | "skipped"))
                })
            })
        })
    {
        return r::fail("tick receipt or wake ledger is malformed; prior receipt retained");
    }
    Ok(())
}
fn kept_receipt(target: &std::path::Path) -> Result<Value> {
    if !r::exists(target) {
        return Ok(json!({}));
    }
    let row = r::parse(&r::read(target, 1024 * 1024)?)?;
    validate_wakes(&row)?;
    Ok(row)
}
fn valid_maintenance(row: &Value) -> bool {
    let Some(table) = row.as_object() else {
        return false;
    };
    let allowed = [
        "journal_rc",
        "ci_rc",
        "plugin_rc",
        "threads_closed",
        "tasks_closed",
        "plans_retired",
        "plan_retire_rc",
        "artifacts_resolved",
        "artifact_resolve_rc",
        "artifacts_superseded",
        "artifact_supersede_rc",
        "failures_retired",
    ];
    table.keys().all(|k| allowed.contains(&k.as_str()))
        && [
            "journal_rc",
            "ci_rc",
            "plugin_rc",
            "plan_retire_rc",
            "artifact_resolve_rc",
            "artifact_supersede_rc",
        ]
        .iter()
        .all(|k| row[*k] == 0 || row[*k] == "skipped")
        && [
            "tasks_closed",
            "plans_retired",
            "artifacts_resolved",
            "artifacts_superseded",
            "failures_retired",
        ]
        .iter()
        .all(|k| row[*k].as_u64().is_some())
        && row["threads_closed"]
            .as_array()
            .is_some_and(|a| a.iter().all(|v| v.as_str().is_some_and(r::safe_name)))
}

fn maintenance_active(target: &std::path::Path, cwd: &std::path::Path) -> Result<bool> {
    let mutex = r::file_lock(target, cwd)?;
    let mut name = mutex.path().as_os_str().to_owned();
    name.push(".checks");
    let checks = PathBuf::from(name);
    if !r::exists(&checks) {
        return Ok(false);
    }
    if std::fs::symlink_metadata(&checks)?.file_type().is_symlink() {
        return Ok(true);
    }
    for entry in std::fs::read_dir(checks)? {
        let entry = entry?;
        if !r::read(&entry.path().join("owner"), 4096)
            .is_ok_and(|v| v.iter().any(|b| !b.is_ascii_whitespace()))
        {
            return Ok(true);
        }
        let mut settings = oms_lock::Settings::from_env()
            .map_err(|_| r::Error(2, "cannot inspect verification marker".into()))?;
        settings.cwd = cwd.into();
        settings.mode = oms_lock::Mode::Mkdir;
        let marker = oms_lock::Lock::at_path(entry.path(), settings)
            .map_err(|_| r::Error(2, "unsafe verification marker".into()))?;
        if marker.probe() != oms_lock::Status::Free {
            return Ok(true);
        }
    }
    Ok(false)
}
