use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

fn state_path(paths: &Paths) -> Result<PathBuf> {
    r::leaf(
        &std::env::var_os("OH_MY_SETTING_AUTO_UPDATE_STATE")
            .map(PathBuf::from)
            .unwrap_or(paths.source.join("local/auto-update.status")),
    )
}
fn read_state(paths: &Paths) -> Result<Option<Value>> {
    let file = state_path(paths)?;
    if !r::exists(&file) {
        return Ok(None);
    }
    let raw = r::read(&file, r::LIMIT)?;
    if raw.first() == Some(&b'{') {
        return Ok(Some(r::parse(&raw)?));
    }
    let text = std::str::from_utf8(&raw)
        .map_err(|_| r::Error(2, "auto-update state is not UTF-8".into()))?;
    let mut row = serde_json::Map::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if matches!(
                key,
                "last_run"
                    | "mode"
                    | "status"
                    | "message"
                    | "source_root"
                    | "local"
                    | "remote"
                    | "upstream"
            ) {
                row.entry(key).or_insert(json!(value));
            }
        }
    }
    Ok(Some(Value::Object(row)))
}
fn publish(
    paths: &Paths,
    status: &str,
    current: &str,
    target: &str,
    reference: &str,
) -> Result<()> {
    let path = state_path(paths)?;
    let _guard = r::lock(&path, &paths.cwd)?;
    let source = paths
        .source
        .to_str()
        .filter(|s| !s.contains(['\r', '\n']))
        .ok_or_else(|| {
            r::Error(
                2,
                "auto-update state requires a UTF-8 source path without newlines".into(),
            )
        })?;
    let message = match status {
        "up_to_date" => "already up to date",
        "failed" => "update check failed",
        _ => "update available",
    };
    let body = format!(
        "last_run={}\nmode=check\nstatus={status}\nmessage={message}\nsource_root={source}\nlocal={current}\nremote={target}\nupstream={reference}\n",
        r::now()
    );
    r::atomic(&path, body.as_bytes())
}
fn wired(paths: &Paths) -> Result<(bool, bool)> {
    let units = paths.config.parent().unwrap().join("systemd/user");
    let enabled = units.join("timers.target.wants/oh-my-setting-autoupdate.timer");
    if enabled.is_symlink()
        && r::physical(&enabled)? == r::physical(&units.join("oh-my-setting-autoupdate.timer"))?
    {
        return Ok((true, true));
    }
    let raw = if let Some(file) = std::env::var_os("OH_MY_SETTING_AUTO_UPDATE_CRON_FILE") {
        let file = PathBuf::from(file);
        if r::exists(&file) {
            Some(r::read(&file, r::LIMIT)?)
        } else {
            None
        }
    } else {
        crate::effects::cron(&paths.cwd).ok().flatten()
    };
    let cron = raw.is_some_and(|b| {
        String::from_utf8_lossy(&b).lines().any(|l| {
            !l.trim_start().starts_with('#')
                && (l.contains("auto-update.sh") || l.contains(" auto-update "))
        })
    });
    Ok((cron, false))
}
fn attention(paths: &Paths, flag: &AtomicBool) -> Result<&'static str> {
    let Some(owner) = paths.load()? else {
        return Ok("disabled");
    };
    if crate::receipt::owner(&owner)? != paths.source || owner["components"]["auto_update"] == false
    {
        return Ok("disabled");
    }
    let (wired, systemd) = wired(paths)?;
    if !wired {
        return Ok("unwired");
    }
    let Some(state) = read_state(paths)? else {
        return Ok("no-run");
    };
    let status = state["status"].as_str().unwrap_or("");
    if status.is_empty() {
        return Ok("unknown");
    }
    let last = state["last_run"]
        .as_str()
        .or_else(|| state["checked_at"].as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let installed = owner["installed_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let superseded = installed.zip(last).is_some_and(|(a, b)| a > b);
    if !superseded && status == "failed" {
        return Ok("failed");
    }
    if !superseded && status == "skipped" {
        return Ok("blocked");
    }
    if systemd {
        if let Some(program) = r::find("loginctl", &paths.cwd) {
            let output = r::command(
                program,
                &[
                    "show-user".into(),
                    rustix::process::geteuid().as_raw().to_string().into(),
                    "--property=Linger".into(),
                    "--value".into(),
                ],
                &paths.cwd,
                None,
                flag,
                Duration::from_secs(5),
            )?;
            if output.0 == 0 && output.1.strip_suffix(b"\n").unwrap_or(&output.1) == b"no" {
                return Ok("session-only");
            }
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339(&r::now())
        .map_err(|_| r::Error(2, "clock unavailable".into()))?;
    if last.is_some_and(|t| now.signed_duration_since(t).num_seconds() >= 172800) {
        return Ok("overdue");
    }
    if last.is_none() {
        return Ok("unknown");
    }
    Ok("ok")
}
fn fetch(paths: &Paths, reference: &str, fallback: bool, flag: &AtomicBool) -> Result<String> {
    if reference != "edge" {
        let fetched = r::git(
            &paths.source,
            &["fetch", "--quiet", "--", "origin", reference],
            flag,
        );
        if fetched.is_ok() {
            return r::git(
                &paths.source,
                &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
                flag,
            );
        }
        r::check(flag)?;
        if !fallback {
            return r::fail(
                "pinned update ref did not resolve; use --fallback-to-edge to follow the default branch",
            );
        }
    }
    r::git(&paths.source, &["fetch", "--quiet", "--", "origin"], flag)?;
    r::git(&paths.source, &["remote", "set-head", "origin", "-a"], flag)?;
    r::git(
        &paths.source,
        &["rev-parse", "--verify", "refs/remotes/origin/HEAD^{commit}"],
        flag,
    )
}
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.tool == "auto-update" {
        match options.action.as_str() {
            "install" | "remove" => {
                let mut o = options.clone();
                o.tool = if options.action == "install" {
                    "install-autoupdate"
                } else {
                    "uninstall-autoupdate"
                }
                .into();
                return crate::scheduler::run(paths, &o, flag);
            }
            "status" => {
                let Some(state) = read_state(paths)? else {
                    return Ok(r::text("auto-update: not checked"));
                };
                if options.has("--json") {
                    return r::json(&state);
                }
                let mut out = format!(
                    "auto-update: {}\nlast run: {}",
                    state["status"].as_str().unwrap_or("unknown"),
                    state["last_run"]
                        .as_str()
                        .or_else(|| state["checked_at"].as_str())
                        .unwrap_or("unknown")
                );
                for key in ["upstream", "local", "remote"] {
                    if let Some(value) = state[key].as_str() {
                        out.push_str(&format!(
                            "\n{key}: {}",
                            value
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(160)
                                .collect::<String>()
                        ));
                    }
                }
                return Ok(r::text(out));
            }
            "attention" => {
                let verdict = attention(paths, flag).unwrap_or("unknown");
                return if options.has("--json") {
                    r::json(&json!({"attention":verdict}))
                } else {
                    Ok(r::text(format!("attention: {verdict}")))
                };
            }
            "" | "check" | "apply" => (),
            _ => return r::fail("unknown auto-update mode"),
        }
    }
    let old = paths.require_owner()?;
    let reference = options
        .get("--ref")
        .map(str::to_owned)
        .or_else(|| {
            old.as_ref()
                .and_then(|r| r["ref"].as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| "edge".into());
    if reference.is_empty() || reference.starts_with('-') || reference.contains(['\n', '\r', '\0'])
    {
        return r::fail("invalid update ref");
    }
    let current = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    if options.preview() {
        return r::json(
            &json!({"schema":1,"current":current,"ref":reference,"actions":["fetch target","verify clean checkout and worker/call fences","verify native payload","reconcile recorded components","doctor gate","publish receipt"],"not_previewed":["fetch","provider refresh"]}),
        );
    }
    r::child_guard()?;
    if options.has("--probe-agy-surfaces") {
        return r::fail(
            "native Antigravity surface certification service unavailable; no provider was started",
        );
    }
    let check =
        options.has("--check") || options.tool == "auto-update" && options.action != "apply";
    if !check {
        return r::fail(
            "incoming-revision native dispatch service unavailable; checkout and installation unchanged",
        );
    }
    let raw = r::git(&paths.source, &["rev-parse", "--git-common-dir"], flag)?;
    let raw = PathBuf::from(raw);
    let file = if raw.is_absolute() {
        raw
    } else {
        paths.source.join(raw)
    };
    let file = file.join("FETCH_HEAD");
    let _fetch_guard = r::lock(&file, &paths.cwd)?;
    let current = r::git(&paths.source, &["rev-parse", "HEAD"], flag)?;
    let target = match fetch(paths, &reference, options.has("--fallback-to-edge"), flag) {
        Ok(target) => target,
        Err(error) => {
            if options.tool == "auto-update" {
                publish(paths, "failed", &current, "", &reference)?;
            }
            return Err(error);
        }
    };
    if target.len() != 40 || !target.bytes().all(|b| b.is_ascii_hexdigit()) {
        return r::fail("invalid fetched update commit");
    }
    let result = json!({"schema":1,"checked_at":r::now(),"current_commit":current,"target_commit":target,"update_available":current!=target,"ref":reference});
    if options.tool == "auto-update" {
        publish(
            paths,
            if current == target {
                "up_to_date"
            } else {
                "update_available"
            },
            &current,
            &target,
            &reference,
        )?;
    }
    if options.has("--json") {
        r::json(&result)
    } else {
        Ok(r::text(format!(
            "update-check: {} {current}{}",
            if current == target {
                "up_to_date"
            } else {
                "available"
            },
            if current == target {
                String::new()
            } else {
                format!(" -> {target}")
            }
        )))
    }
}
