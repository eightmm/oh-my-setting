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
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    let action = if options.action.is_empty() {
        "run"
    } else {
        options.action.as_str()
    };
    if matches!(action, "install" | "uninstall") {
        return crate::scheduler::run(paths, options, flag);
    }
    if action == "schedule" {
        return r::fail("native contract schedule authority adapter is not yet available");
    }
    if action == "refresh-catalogs" {
        return r::fail("native provider catalog refresh adapter is not yet available");
    }
    let mut repos = options
        .all("--repo")
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
                    .or_else(|_| r::physical(p))
            })
            .collect::<Result<Vec<_>>>()?;
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
    let mut receipts = Vec::new();
    for repo in repos {
        r::check(flag)?;
        if !repo.join(".oms").is_dir() {
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
        let args = [
            "--repo".into(),
            repo.as_os_str().to_owned(),
            "reconcile".into(),
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
        let row = json!({"schema":1,"ran_at":r::now(),"repo":repo,"status":if reconcile.0==0{"partial"}else{"failed"},"journal_rc":null,"reconcile_rc":reconcile.0,"memory_rc":memory,"telemetry_rc":telemetry.0,"gc_rc":null,"threads_closed":[],"tasks_closed":0,"plans_retired":0,"artifacts_resolved":0,"artifacts_superseded":0,"failures_retired":0,"wake":"skipped","unavailable":["journal sync","CI sync","thread/task retirement","plan retirement","artifact resolution","failure retirement","catalog refresh","idle-main wake"]});
        r::check(flag)?;
        r::atomic(&target, &r::bytes(&row)?)?;
        receipts.push(row);
        drop(guard);
    }
    r::json(&json!({"schema":1,"sweeps":receipts}))
}
