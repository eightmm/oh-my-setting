use crate::{
    cli::Options,
    receipt::Paths,
    runtime::{self as r, Result},
    transaction::Transaction,
};
use serde_json::json;
use std::{ffi::OsString, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

fn invoke(
    paths: &Paths,
    name: &str,
    args: &[&str],
    input: Option<&[u8]>,
    flag: &AtomicBool,
) -> Result<crate::Output> {
    let program =
        r::find(name, &paths.cwd).ok_or_else(|| r::Error(2, format!("{name} is unavailable")))?;
    r::command(
        program,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        &paths.cwd,
        input,
        flag,
        Duration::from_secs(30),
    )
}
fn block(raw: &str, name: &str, replacement: &str) -> Result<String> {
    let begin = format!("# oh-my-setting {name}:begin");
    let end = format!("# oh-my-setting {name}:end");
    let mut inside = false;
    let mut pairs = 0;
    let mut result = String::new();
    for line in raw.lines() {
        if line == begin {
            if inside || pairs > 0 {
                return r::fail("malformed scheduler ownership markers");
            }
            inside = true;
        } else if line == end {
            if !inside {
                return r::fail("orphan scheduler end marker");
            }
            inside = false;
            pairs += 1;
        } else if !inside {
            result.push_str(line);
            result.push('\n');
        }
    }
    if inside {
        return r::fail("orphan scheduler begin marker");
    }
    if !replacement.is_empty() {
        result.push_str(&format!("{begin}\n{replacement}\n{end}\n"));
    }
    Ok(result)
}
/// Accepts native units marked with `# oh-my-setting native lifecycle` as well
/// as legacy shell-era OMS systemd units pointing strictly at this checkout.
fn owned_systemd_unit(raw: &str, timer: bool, paths: &Paths, tick: bool) -> bool {
    if raw.contains("# oh-my-setting native lifecycle") {
        return true;
    }
    if timer {
        let mut has_calendar = false;
        let mut has_wanted = false;
        for line in raw.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            match l {
                "[Unit]" | "[Timer]" | "[Install]" | "Persistent=true" => {}
                "WantedBy=timers.target" => has_wanted = true,
                _ if l.starts_with("RandomizedDelaySec=") => {}
                "Description=Run oh-my-setting auto-update daily"
                | "Description=oh-my-setting autoupdate"
                    if !tick => {}
                "OnCalendar=daily" if !tick => has_calendar = true,
                "Description=Run oh-my-setting tick hourly" | "Description=oh-my-setting tick"
                    if tick => {}
                "OnCalendar=hourly" if tick => has_calendar = true,
                _ => return false,
            }
        }
        return has_calendar && has_wanted;
    }
    let expected_script = paths.source.join(if tick {
        "scripts/tick.sh"
    } else {
        "scripts/auto-update.sh"
    });
    let mut exec_count = 0usize;
    for line in raw.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if matches!(l, "[Unit]" | "[Service]" | "Type=oneshot") {
            continue;
        }
        if !tick
            && matches!(
                l,
                "Description=oh-my-setting auto-update" | "Description=oh-my-setting autoupdate"
            )
            || tick && l == "Description=oh-my-setting tick"
        {
            continue;
        }
        if let Some(rest) = l.strip_prefix("Environment=") {
            let inner = rest
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(rest);
            let Some((var, _)) = inner.split_once('=') else {
                return false;
            };
            if var != "PATH" && !var.starts_with("OH_MY_SETTING_") && !var.starts_with("OMS_") {
                return false;
            }
            continue;
        }
        if let Some(cmd) = l.strip_prefix("ExecStart=") {
            exec_count += 1;
            let Some(words) = crate::integrations::command_words(cmd) else {
                return false;
            };
            let arg_ok = |arg: &str| {
                if tick {
                    arg == "run"
                } else {
                    matches!(arg, "apply" | "check")
                }
            };
            let valid = match words
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                [script, arg] => std::path::Path::new(script) == expected_script && arg_ok(arg),
                [bash, script, arg] => {
                    (*bash == "bash"
                        || std::path::Path::new(bash)
                            .file_name()
                            .is_some_and(|n| n == "bash"))
                        && std::path::Path::new(script) == expected_script
                        && arg_ok(arg)
                }
                _ => false,
            };
            if !valid {
                return false;
            }
            continue;
        }
        return false;
    }
    exec_count == 1
}

pub(crate) fn apply(
    paths: &Paths,
    options: &Options,
    tx: &mut Transaction,
    flag: &AtomicBool,
) -> Result<crate::Output> {
    let tick = options.tool == "tick";
    let name = if tick { "tick" } else { "autoupdate" };
    let remove = options.tool == "uninstall-autoupdate"
        || options.action == "uninstall"
        || options.has("--remove");
    let method = options.get("--method").unwrap_or("auto");
    if !matches!(method, "auto" | "systemd" | "cron") {
        return r::fail("--method must be auto, systemd, or cron");
    }
    let mode = if options.has("--check-only") {
        "check"
    } else {
        "apply"
    };
    let cron_file = std::env::var_os("OH_MY_SETTING_AUTO_UPDATE_CRON_FILE").map(PathBuf::from);
    let units = paths.config.parent().unwrap().join("systemd/user");
    let existing_units = r::exists(&units.join(format!("oh-my-setting-{name}.service")))
        || r::exists(&units.join(format!("oh-my-setting-{name}.timer")));
    let user_manager_ready = r::find("systemctl", &paths.cwd).is_some()
        && (method == "systemd"
            || std::env::var_os("XDG_RUNTIME_DIR").is_some()
                && invoke(
                    paths,
                    "systemctl",
                    &["--user", "show-environment"],
                    None,
                    flag,
                )
                .is_ok_and(|o| o.0 == 0));
    let systemd = if remove {
        method == "systemd" || (method != "cron" && existing_units)
    } else {
        method == "systemd" || (method == "auto" && cron_file.is_none() && user_manager_ready)
    };
    if options.preview() {
        return Ok(r::text(format!(
            "would {} {name} {} trigger",
            if remove { "remove owned" } else { "install" },
            if systemd { "systemd" } else { "cron" }
        )));
    }
    let executable = paths.home.join(".local/bin/oms");
    let command = if tick {
        format!("{} tick run", r::shell(executable.as_os_str())?)
    } else {
        format!("{} auto-update {mode}", r::shell(executable.as_os_str())?)
    };
    if systemd {
        let dir = paths.config.parent().unwrap().join("systemd/user");
        let service = dir.join(format!("oh-my-setting-{name}.service"));
        let timer = dir.join(format!("oh-my-setting-{name}.timer"));
        for (path, is_timer) in [(&service, false), (&timer, true)] {
            if r::exists(path) {
                let raw = r::read(path, r::LIMIT)?;
                if !owned_systemd_unit(&String::from_utf8_lossy(&raw), is_timer, paths, tick) {
                    return r::fail("refusing takeover of an unmanaged scheduler unit");
                }
            }
        }
        if !remove || user_manager_ready {
            tx.effect(crate::effects::Effect::systemd(&paths.cwd, name)?)?;
        }
        if remove {
            if user_manager_ready && (r::exists(&timer) || method == "systemd") {
                let result = invoke(
                    paths,
                    "systemctl",
                    &[
                        "--user",
                        "disable",
                        "--now",
                        &format!("oh-my-setting-{name}.timer"),
                    ],
                    None,
                    flag,
                )?;
                if result.0 != 0 {
                    let stderr = String::from_utf8_lossy(&result.2).to_lowercase();
                    if !stderr.contains("not exist") && !stderr.contains("not loaded") {
                        return r::fail("could not disable the owned user timer");
                    }
                }
            }
            tx.remove(&service)?;
            tx.remove(&timer)?;
        } else {
            // Systemd has its own quoting language. Restrict to paths whose
            // literal form survives it; do not feed shell quoting to ExecStart.
            let command_path = executable
                .to_str()
                .ok_or_else(|| r::Error(2, "timer executable path is not UTF-8".into()))?;
            if command_path.contains(['%', '"', '\\', '\n', '\r']) {
                return r::fail("timer executable path needs unsupported systemd escaping");
            }
            let arguments = if tick {
                "tick run".to_owned()
            } else {
                format!("auto-update {mode}")
            };
            tx.write(&service,format!("# oh-my-setting native lifecycle\n[Unit]\nDescription=oh-my-setting {name}\n[Service]\nType=oneshot\nExecStart=\"{command_path}\" {arguments}\n").as_bytes(),0o600)?;
            tx.write(&timer,format!("# oh-my-setting native lifecycle\n[Unit]\nDescription=oh-my-setting {name}\n[Timer]\nOnCalendar={}\nPersistent=true\n[Install]\nWantedBy=timers.target\n",if tick{"hourly"}else{"daily"}).as_bytes(),0o600)?;
        }
        if (!remove || user_manager_ready)
            && invoke(paths, "systemctl", &["--user", "daemon-reload"], None, flag)?.0 != 0
        {
            return r::fail("user manager reload failed; unit file transaction rolls back");
        }
        if !remove
            && invoke(
                paths,
                "systemctl",
                &[
                    "--user",
                    "enable",
                    "--now",
                    &format!("oh-my-setting-{name}.timer"),
                ],
                None,
                flag,
            )?
            .0 != 0
        {
            return r::fail("user timer activation failed; unit file transaction rolls back");
        }
    }
    if (!systemd || remove)
        && (!remove || cron_file.is_some() || r::find("crontab", &paths.cwd).is_some())
    {
        let old = if let Some(file) = &cron_file {
            if r::exists(file) {
                String::from_utf8(r::read(file, r::LIMIT)?)
                    .map_err(|_| r::Error(2, "crontab is not UTF-8".into()))?
            } else {
                String::new()
            }
        } else {
            let out = invoke(paths, "crontab", &["-l"], None, flag)?;
            if out.0 != 0
                && !String::from_utf8_lossy(&out.2)
                    .to_lowercase()
                    .contains("no crontab")
            {
                return r::fail("could not read crontab; refusing to replace unknown jobs");
            }
            String::from_utf8(out.1).map_err(|_| r::Error(2, "crontab is not UTF-8".into()))?
        };
        if command.contains('%') {
            return r::fail("cron command contains an unescaped percent sign");
        }
        let replacement = if remove {
            String::new()
        } else {
            format!(
                "{} {} >/dev/null 2>&1",
                if tick { "17 * * * *" } else { "17 4 * * *" },
                command
            )
        };
        let legacy = if tick {
            retire_legacy_tick(&old, paths)?
        } else {
            old.clone()
        };
        let new = block(&legacy, name, &replacement)?;
        if let Some(file) = cron_file {
            if !(remove && !r::exists(&file) && new.is_empty()) {
                tx.write(&file, new.as_bytes(), 0o600)?;
            }
        } else if !(remove && new == old) {
            let before = crate::effects::cron(&paths.cwd)?;
            if before.as_deref().unwrap_or(&[]) != old.as_bytes() {
                return r::fail("crontab changed before publication");
            }
            tx.effect(crate::effects::Effect::Crontab {
                cwd: paths.cwd.clone(),
                before,
                after: new.as_bytes().to_vec(),
            })?;
            if invoke(paths, "crontab", &["-"], Some(new.as_bytes()), flag)?.0 != 0 {
                return r::fail(
                    "crontab installation failed; rollback will restore the previous jobs",
                );
            }
        }
    }
    if let Some(mut row) = paths.load()? {
        row["components"][if tick { "tick" } else { "auto_update" }] = json!(!remove);
        if !tick {
            row["component_modes"]["auto_update"] = json!(mode);
        }
        tx.write(&paths.receipt, &r::bytes(&row)?, 0o600)?;
    }
    Ok(r::text(format!(
        "{name} trigger: {}",
        if remove { "removed" } else { "installed" }
    )))
}

pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
    if options.preview() {
        return Ok(r::text("would converge the owned scheduler trigger"));
    }
    r::child_guard()?;
    let _guard = r::receipt_lock(paths, flag)?;
    paths.require_owner()?;
    let mut tx = Transaction::begin(paths)?;
    let result = apply(paths, options, &mut tx, flag)?;
    r::check(flag)?;
    tx.commit()?;
    Ok(result)
}

fn retire_legacy_tick(raw: &str, paths: &Paths) -> Result<String> {
    let script = paths.source.join("scripts/tick.sh");
    let quoted = format!(
        "\"{}\" run >/dev/null 2>&1 # oh-my-setting tick",
        script.to_str().ok_or_else(|| r::Error(
            2,
            "legacy cron migration requires a UTF-8 source path".into()
        ))?
    );
    let mut out = String::new();
    for line in raw.lines() {
        if line.ends_with("# oh-my-setting tick") && !line.trim_start().starts_with('#') {
            if !line.starts_with("23 * * * * ")
                || !line.ends_with(&quoted)
                || line.contains(['\n', '\r'])
            {
                return r::fail("ambiguous legacy tick cron entry; crontab retained");
            }
            let prefix = line
                .strip_suffix(&quoted)
                .unwrap()
                .strip_prefix("23 * * * * ")
                .unwrap();
            // Do not withdraw a line whose prefix can run another command.
            if !prefix.is_empty() {
                let rest = prefix
                    .strip_prefix("PATH=")
                    .and_then(|s| s.split_once(' '))
                    .filter(|(path, _)| {
                        !path.is_empty()
                            && path
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_./:+-".contains(&b))
                    })
                    .map(|(_, rest)| rest)
                    .ok_or_else(|| {
                        r::Error(
                            2,
                            "ambiguous legacy tick PATH prefix; crontab retained".into(),
                        )
                    })?;
                if !rest.is_empty() {
                    let recorded =
                        String::from_utf8(r::read(&paths.config.join("bash-interpreter"), 4096)?)
                            .map_err(|_| r::Error(2, "invalid recorded Bash path".into()))?;
                    if rest != format!("\"{}\" ", recorded.trim()) {
                        return r::fail("unrecognized legacy tick interpreter; crontab retained");
                    }
                }
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(source: &str) -> Paths {
        Paths {
            home: PathBuf::from("/tmp/home"),
            source: PathBuf::from(source),
            receipt: PathBuf::from("/tmp/receipt"),
            config: PathBuf::from("/tmp/config"),
            state: PathBuf::from("/tmp/state"),
            data: PathBuf::from("/tmp/data"),
            cwd: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn owned_systemd_unit_accepts_legacy_oms_units_and_rejects_foreign_units() {
        let paths = test_paths("/opt/oms-checkout");
        let legacy_auto_service = concat!(
            "[Unit]\nDescription=oh-my-setting auto-update\n\n",
            "[Service]\nType=oneshot\nEnvironment=OH_MY_SETTING_MCP=1\n",
            "ExecStart=\"/usr/bin/bash\" \"/opt/oms-checkout/scripts/auto-update.sh\" apply\n",
        );
        let legacy_auto_timer = concat!(
            "[Unit]\nDescription=Run oh-my-setting auto-update daily\n\n",
            "[Timer]\nOnCalendar=daily\nPersistent=true\nRandomizedDelaySec=30m\n\n",
            "[Install]\nWantedBy=timers.target\n",
        );
        let legacy_tick_service = concat!(
            "[Unit]\nDescription=oh-my-setting tick\n[Service]\nType=oneshot\n",
            "Environment=PATH=/usr/bin:/bin\n",
            "ExecStart=\"/opt/oms-checkout/scripts/tick.sh\" run\n",
        );
        let legacy_tick_timer = concat!(
            "[Unit]\nDescription=Run oh-my-setting tick hourly\n[Timer]\n",
            "OnCalendar=hourly\nPersistent=true\nRandomizedDelaySec=10m\n",
            "[Install]\nWantedBy=timers.target\n",
        );
        assert!(owned_systemd_unit(
            legacy_auto_service,
            false,
            &paths,
            false
        ));
        assert!(owned_systemd_unit(legacy_auto_timer, true, &paths, false));
        assert!(owned_systemd_unit(legacy_tick_service, false, &paths, true));
        assert!(owned_systemd_unit(legacy_tick_timer, true, &paths, true));
        assert!(owned_systemd_unit(
            "# oh-my-setting native lifecycle\n[Unit]\n",
            false,
            &paths,
            false
        ));
        assert!(!owned_systemd_unit(
            "[Service]\nExecStart=\"/usr/bin/custom\" run\n",
            false,
            &paths,
            false
        ));
        assert!(!owned_systemd_unit(
            "[Service]\nEnvironment=LD_PRELOAD=/tmp/x.so\nExecStart=\"/opt/oms-checkout/scripts/tick.sh\" run\n",
            false,
            &paths,
            true
        ));
        assert!(!owned_systemd_unit(
            "[Unit]\nDescription=other\n[Timer]\nOnCalendar=daily\n[Install]\nWantedBy=timers.target\n",
            true,
            &paths,
            false
        ));
    }
}
