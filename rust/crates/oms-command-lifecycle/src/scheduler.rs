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
pub(crate) fn run(paths: &Paths, options: &Options, flag: &AtomicBool) -> Result<crate::Output> {
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
    let systemd = method == "systemd"
        || method == "auto"
            && cron_file.is_none()
            && r::find("systemctl", &paths.cwd).is_some()
            && std::env::var_os("XDG_RUNTIME_DIR").is_some()
            && invoke(
                paths,
                "systemctl",
                &["--user", "show-environment"],
                None,
                flag,
            )
            .is_ok_and(|o| o.0 == 0);
    if options.preview() {
        return Ok(r::text(format!(
            "would {} {name} {} trigger",
            if remove { "remove owned" } else { "install" },
            if systemd { "systemd" } else { "cron" }
        )));
    }
    r::child_guard()?;
    paths.require_owner()?;
    let _guard = r::lock(&paths.receipt, &paths.cwd)?;
    paths.require_owner()?;
    let mut tx = Transaction::begin(paths)?;
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
        for path in [&service, &timer] {
            if r::exists(path) {
                let raw = r::read(path, r::LIMIT)?;
                if !String::from_utf8_lossy(&raw).contains("# oh-my-setting native lifecycle") {
                    return r::fail("refusing takeover of an unmanaged scheduler unit");
                }
            }
        }
        tx.effect(crate::effects::Effect::systemd(&paths.cwd, name)?)?;
        if remove {
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
                return r::fail("could not disable the owned user timer");
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
        if invoke(paths, "systemctl", &["--user", "daemon-reload"], None, flag)?.0 != 0 {
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
    } else {
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
        let new = block(&old, name, &replacement)?;
        if let Some(file) = cron_file {
            tx.write(&file, new.as_bytes(), 0o600)?;
        } else {
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
    tx.commit()?;
    Ok(r::text(format!(
        "{name} trigger: {}",
        if remove { "removed" } else { "installed" }
    )))
}
